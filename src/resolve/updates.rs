//! The update status: one pure answer to "is there an update for this addon on this
//! client?", shared by `outdated`, `update`, `unpin`'s hint, `status` counts and `list`
//! (M2-D1, M2-D3). It reads the record, the release memo's entry, the candidate facts
//! and the target, and nothing else: no I/O and no clock, so `status --cached` can run
//! it on every refresh and every edge case is a table test.
//!
//! The order of the checks matters: a static install has nothing to check; a source
//! that is off, a missing entry or a remembered failure stop early; the installed
//! release being the newest is `current` even when pinned; the downgrade guard runs
//! next, so a pin ahead of the newest release is `ahead` (`pinned` says a newer release
//! exists); a pin then holds before any fact is compared; a skip mark then holds the
//! release that would be offered, before it is judged blocked; and every confidence counts as
//! an update, because trying it and recording the verdict corrects the badge after at
//! most one attempt.

use std::cmp::Ordering;
use std::collections::BTreeSet;

use jiff::Timestamp;
use schemars::JsonSchema;
use serde::Serialize;

use crate::error::{Error, ErrorBody, Suggestion};
use crate::model::{
    AddonRecord, Candidate, Channel, RankReason, Ref, Release, ReleaseKey, SkippedRelease, Target,
    VersionConfidence, skip_label,
};
use crate::resolve::select::select;
use crate::rules::{Family, RulesTable};
use crate::sources::{LookBack, SourceId};
use crate::state::memo::{FactIndex, MemoEntry};
use crate::state::records::TargetRecord;
use crate::toc::{DiskFolder, loaded_toc, primary_interface};

/// Whether an installed addon has an update on its client.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum UpdateStatus {
    /// The newest release the channel admits is installed.
    Current { latest: ReleaseBrief },
    /// A newer release exists, with how likely it is to load here.
    UpdateAvailable {
        latest: ReleaseBrief,
        confidence: Confidence,
    },
    /// Pinned: `update` leaves it where it is.
    Pinned { latest: Option<ReleaseBrief> },
    /// The release that would be offered is skipped on this client: `update` passes
    /// over it and resumes at the next one. `skipped_release` is the mark that holds it.
    SkippedRelease {
        latest: ReleaseBrief,
        skipped_release: SkippedRelease,
    },
    /// The installed release is newer than the newest one the channel admits.
    Ahead { latest: ReleaseBrief },
    /// The newest release is known not to suit this client.
    Blocked {
        latest: ReleaseBrief,
        #[serde(rename = "blocked_reason")]
        reason: BlockedReason,
    },
    /// The source said, in a way retrying does not change, that it cannot answer.
    Failed { error: ErrorBody },
    /// Not known.
    Unchecked {
        #[serde(rename = "unchecked_reason")]
        reason: UncheckedReason,
    },
    /// A local or URL install, or one from a given file: nothing to check.
    Static,
}

/// How likely an update is to load on the client.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    /// A plan verified a candidate loads here.
    Verified,
    /// The publisher's metadata names this client.
    Declared,
    /// Nothing says otherwise.
    Likely,
    /// The metadata names other clients only.
    Unlikely,
}

/// Why the newest release is blocked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BlockedReason {
    /// Every candidate of it was verified not to load on this client.
    VerifiedNotLoading,
    /// It targets an older interface than the adopted build installed.
    OlderThanInstalled,
}

/// Why nothing is known.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum UncheckedReason {
    NeverChecked,
    SourceDisabled,
    Offline,
    /// No release on the addon's channel has an installable asset for this client.
    NoEligibleRelease,
}

/// A release, as statuses name it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ReleaseBrief {
    pub version: String,
    pub release_id: String,
    #[schemars(schema_with = "crate::contract::schema::timestamp_opt")]
    pub published_at: Option<Timestamp>,
    pub channel: Channel,
    /// The release's source revision, when its source has one (WowAce). Omitted
    /// otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision: Option<u32>,
    /// The installed release, at another revision: its tag was moved upstream. Omitted
    /// otherwise.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub retagged: bool,
    /// The file name of the candidate the status was judged on (the one `update`
    /// tries first); for `list`'s update entry, never serialized.
    #[serde(skip)]
    pub asset: Option<String>,
}

impl ReleaseBrief {
    /// The version as a line shows it: `v12.1.0 (re-tagged upstream)` for a re-tag.
    pub fn shown(&self) -> String {
        if self.retagged {
            format!("{} (re-tagged upstream)", self.version)
        } else {
            self.version.clone()
        }
    }
}

/// The name of a status, as the wire spells it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StatusKind {
    Current,
    UpdateAvailable,
    Pinned,
    SkippedRelease,
    Ahead,
    Blocked,
    Failed,
    Unchecked,
    Static,
}

impl UpdateStatus {
    pub const fn kind(&self) -> StatusKind {
        match self {
            Self::Current { .. } => StatusKind::Current,
            Self::UpdateAvailable { .. } => StatusKind::UpdateAvailable,
            Self::Pinned { .. } => StatusKind::Pinned,
            Self::SkippedRelease { .. } => StatusKind::SkippedRelease,
            Self::Ahead { .. } => StatusKind::Ahead,
            Self::Blocked { .. } => StatusKind::Blocked,
            Self::Failed { .. } => StatusKind::Failed,
            Self::Unchecked { .. } => StatusKind::Unchecked,
            Self::Static => StatusKind::Static,
        }
    }

    /// The newest release the status was judged against, when there is one.
    pub const fn latest(&self) -> Option<&ReleaseBrief> {
        match self {
            Self::Current { latest }
            | Self::UpdateAvailable { latest, .. }
            | Self::SkippedRelease { latest, .. }
            | Self::Ahead { latest }
            | Self::Blocked { latest, .. } => Some(latest),
            Self::Pinned { latest } => latest.as_ref(),
            Self::Failed { .. } | Self::Unchecked { .. } | Self::Static => None,
        }
    }
}

/// Whether `record` has nothing upstream to compare (M2-D31): a `file:` or `url:` ref,
/// or a release id naming a given archive or directory (`@sha256:`, `@listing:`, and
/// `@dir:` from records older builds wrote for `--file <dir>`).
pub fn is_static(record: &AddonRecord) -> bool {
    let from_file = record.release_id.as_deref().is_some_and(|id| {
        id.contains("@sha256:") || id.contains("@listing:") || id.contains("@dir:")
    });
    matches!(record.r#ref, Ref::File { .. } | Ref::Url { .. }) || from_file
}

/// The update status of `record` on `target`, from its memo `entry` and the candidate
/// `facts`. `installed_primary_interface` is the installed primary folder's in-family
/// interface (read only for records versioned by their TOC); `source_enabled` says
/// whether the ref's source is on.
pub fn evaluate(
    record: &AddonRecord,
    entry: Option<&MemoEntry>,
    facts: &FactIndex,
    target: &Target,
    installed_primary_interface: Option<u32>,
    source_enabled: bool,
    look_back: &dyn Fn(&str) -> Option<LookBack>,
) -> UpdateStatus {
    // 1. Nothing to check: a local or URL install, or one from a given archive.
    if is_static(record) {
        return UpdateStatus::Static;
    }
    // 2. No usable answer.
    if !source_enabled {
        return unchecked(UncheckedReason::SourceDisabled);
    }
    let Some(entry) = entry else {
        return unchecked(UncheckedReason::NeverChecked);
    };
    let set = match entry.result() {
        Ok(set) => set,
        Err(error) => {
            let fix = Suggestion::command([
                "omaddons",
                "remove",
                record.name.as_str(),
                "-t",
                target.id.as_str(),
            ]);
            let mut error = Error::new(error.code, error.message.clone(), fix);
            // Gone from where it was: it may have moved, and adopting its folder under
            // the new ref re-points it (the ref is the user's to find).
            if matches!(error.code, crate::error::ErrorCode::NotFound)
                && let Some(primary) = crate::engine::plan::primary_folder(
                    &record.r#ref,
                    &record.folders,
                    Some(&record.name),
                )
            {
                error = error.alternative(Suggestion::text(format!(
                    "if it moved, re-point it: omaddons adopt {} --ref <new ref> -t {}",
                    primary.as_str(),
                    target.id
                )));
            }
            return UpdateStatus::Failed {
                error: ErrorBody::from(error),
            };
        }
    };
    // 3. What `update` would try.
    let candidates = select(&set, target, record.channel, None).candidates;
    let Some(best) = candidates.first() else {
        return unchecked(UncheckedReason::NoEligibleRelease);
    };
    let newest = set
        .releases
        .iter()
        .find(|r| r.release_id == best.release_id);
    let mut latest = brief(best, newest);
    // 4. Installed already, unless its tag moved since: the same release id at another
    // revision (WowAce) is a re-tag, an update. A commit elsewhere moves neither.
    if record
        .release_id
        .as_deref()
        .is_some_and(|id| same_release(id, &best.release_id))
    {
        if record.revision == best.revision {
            return UpdateStatus::Current { latest };
        }
        latest.retagged = true;
    }
    // 5. Never a downgrade on its own, pinned or not: `pinned` says a newer release
    // exists, and an older one is no update.
    if installed_is_newer(record, entry, newest, best) {
        return UpdateStatus::Ahead { latest };
    }
    // 6. A pin holds.
    if record.pinned {
        return UpdateStatus::Pinned {
            latest: Some(latest),
        };
    }
    // 7. A skip mark holds the release that would be offered (design 7.5.2): the best
    // candidate's, or the look-back's when it offers an older one instead. Decided after
    // the pin, and before `blocked`: a skipped release that would not load reads
    // `skipped_release`, the person's own decision being the more useful thing to show.
    // 8. What plans learned, under the judge the client has now.
    let verdict = |c: &Candidate| facts.verdict(&c.release_id, &c.asset.filename, target);
    let verdicts: Vec<_> = candidates.iter().map(verdict).collect();
    // Facts judge by the client's own rules; `--any-flavor` installs what they refuse.
    if !record.any_flavor && verdicts.iter().all(|v| v.is_some_and(|v| !v.loads)) {
        // A repository with one release per client: the look-back remembered against
        // this newest release says which is this client's latest.
        if let Some(status) =
            look_back(&best.release_id).and_then(|found| looked_back_status(record, target, &found))
        {
            return held(record, status);
        }
        return held(
            record,
            UpdateStatus::Blocked {
                latest,
                reason: BlockedReason::VerifiedNotLoading,
            },
        );
    }
    if let Some(mark) = covering(record, &latest) {
        return UpdateStatus::SkippedRelease {
            skipped_release: mark.clone(),
            latest,
        };
    }
    let loading = verdicts.iter().flatten().find(|v| v.loads);
    let by_toc = record.version_confidence == VersionConfidence::Toc;
    if by_toc
        && let Some(fact) = loading
        && (fact.out_of_date
            || matches!(
                (fact.primary_interface, installed_primary_interface),
                (Some(candidate), Some(installed)) if candidate < installed
            ))
    {
        return UpdateStatus::Blocked {
            latest,
            reason: BlockedReason::OlderThanInstalled,
        };
    }
    // 9. An update, and how likely it is to load.
    let confidence = match (loading, rank_confidence(best.rank)) {
        (Some(_), _) => Confidence::Verified,
        // Installed with only a TOC version: metadata is no more than likely.
        (None, Confidence::Declared) if by_toc => Confidence::Likely,
        (None, confidence) => confidence,
    };
    UpdateStatus::UpdateAvailable { latest, confidence }
}

/// The mark on `record` that covers the release `latest` names, its revision included
/// ([`skip_covering`] with the release's [`ReleaseKey`]).
pub fn covering<'r>(record: &'r AddonRecord, latest: &ReleaseBrief) -> Option<&'r SkippedRelease> {
    let key = ReleaseKey::parse(&latest.release_id, latest.revision);
    skip_covering(record, key.as_ref(), &latest.version)
}

/// `status` as `record`'s skip marks leave it: a release it would offer (an update, the
/// look-back's included) or reports blocked is `skipped_release` when a mark covers it.
/// `current` offers nothing, so a look-back that finds this client's latest installed
/// stays `current`.
fn held(record: &AddonRecord, status: UpdateStatus) -> UpdateStatus {
    let mark = match &status {
        UpdateStatus::UpdateAvailable { latest, .. } | UpdateStatus::Blocked { latest, .. } => {
            covering(record, latest).cloned()
        }
        _ => None,
    };
    match (mark, status) {
        (
            Some(skipped_release),
            UpdateStatus::UpdateAvailable { latest, .. } | UpdateStatus::Blocked { latest, .. },
        ) => UpdateStatus::SkippedRelease {
            latest,
            skipped_release,
        },
        (_, status) => status,
    }
}

/// The status of `record` against a look-back's answer for its client: `current` when it
/// is the release found, or newer, or out of the feed's reach (a look-back never
/// downgrades), else an update to it, which its `release.json` declares for the client.
fn looked_back_status(
    record: &AddonRecord,
    target: &Target,
    found: &LookBack,
) -> Option<UpdateStatus> {
    let set = found.found.as_ref()?;
    let release = set.releases.first()?;
    let best = select(set, target, record.channel, None)
        .candidates
        .into_iter()
        .next()?;
    let latest = brief(&best, Some(release));
    let tag = |id: &str| id.rsplit_once('@').map(|(_, tag)| tag.to_owned());
    let older = record
        .release_id
        .as_deref()
        .and_then(tag)
        .zip(tag(&release.release_id))
        .is_some_and(|(installed, answer)| older_than_found(&found.order, &installed, &answer));
    Some(if older {
        UpdateStatus::UpdateAvailable {
            latest,
            confidence: Confidence::Declared,
        }
    } else {
        UpdateStatus::Current { latest }
    })
}

/// How likely a release is to load on a client from its best candidate's rank alone (no
/// plan verified it): the publisher's metadata naming the client is `declared`, naming
/// others only `unlikely`, saying nothing `likely`. `outdated` and `info` share it.
pub fn rank_confidence(rank: RankReason) -> Confidence {
    match rank {
        RankReason::MetadataInterface | RankReason::MetadataFlavor => Confidence::Declared,
        RankReason::NameHint | RankReason::Unannotated => Confidence::Likely,
        RankReason::NameHintMismatch | RankReason::MetadataMismatch => Confidence::Unlikely,
    }
}

fn unchecked(reason: UncheckedReason) -> UpdateStatus {
    UpdateStatus::Unchecked { reason }
}

fn brief(best: &Candidate, release: Option<&Release>) -> ReleaseBrief {
    ReleaseBrief {
        version: best.version.clone(),
        release_id: best.release_id.clone(),
        published_at: release.and_then(|r| r.published_at),
        channel: best.channel,
        revision: best.revision,
        retagged: false,
        asset: Some(best.asset.filename.clone()),
    }
}

/// Whether two release ids name one release: `<ref>@<tag>` with the ref compared by its
/// identity (a GitHub owner and repo in any case: an id keeps the spelling of whichever
/// request fetched it, while the memo is shared by identity) and the tag exactly.
pub fn same_release(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    match (a.rsplit_once('@'), b.rsplit_once('@')) {
        (Some((ref_a, tag_a)), Some((ref_b, tag_b))) if tag_a == tag_b => {
            match (ref_a.parse::<Ref>(), ref_b.parse::<Ref>()) {
                (Ok(x), Ok(y)) => x.identity() == y.identity(),
                _ => false,
            }
        }
        _ => false,
    }
}

/// Whether `mark` covers the release that `key` and `version` name (design 7.5.1).
///
/// - An exact mark (`release` present) covers an equal key: the same source in any
///   spelling, the same tag, the same revision. Made without a revision, it holds every
///   revision of its tag, the cautious direction for a hold; with one, it never covers
///   another revision or an unknown one, so a WowAce re-tag is a new release.
/// - A label mark (`release` absent) covers every release whose version is its label,
///   ignoring one leading `v`, whatever its key.
///
/// An exact mark never looks at the version, so an equal label on another tag is not
/// covered.
pub fn skip_covers(mark: &SkippedRelease, key: Option<&ReleaseKey>, version: &str) -> bool {
    match &mark.release {
        Some(marked) => key.is_some_and(|key| {
            marked == key
                || (marked.revision.is_none()
                    && marked.source == key.source
                    && marked.tag == key.tag)
        }),
        None => skip_label(&mark.version) == skip_label(version),
    }
}

/// Whether `unskip --release` of the release `key`, whose version label is `version`,
/// clears `mark` (design 7.5.5): every mark that covers that release ([`skip_covers`]:
/// its exact mark, a mark on its tag made without a revision, a label mark reading its
/// version), and, when `key` names no revision, an exact mark on any revision of its tag,
/// since a release id typed without a revision names the tag. A mark about another tag,
/// or about another revision than a named one, stays.
pub fn unskip_clears(mark: &SkippedRelease, key: &ReleaseKey, version: &str) -> bool {
    skip_covers(mark, Some(key), version)
        || (key.revision.is_none()
            && mark
                .release
                .as_ref()
                .is_some_and(|m| m.source == key.source && m.tag == key.tag))
}

/// The mark on `record` that covers the release these name, if any ([`skip_covers`]).
pub fn skip_covering<'r>(
    record: &'r AddonRecord,
    key: Option<&ReleaseKey>,
    version: &str,
) -> Option<&'r SkippedRelease> {
    record
        .skipped_releases
        .iter()
        .find(|mark| skip_covers(mark, key, version))
}

/// Whether the installed release is newer than the newest one: by publication dates
/// when both are known (the installed one from the entry's releases), else by dotted
/// version numbers when both parse. Unknown either way: not newer.
fn installed_is_newer(
    record: &AddonRecord,
    entry: &MemoEntry,
    newest: Option<&Release>,
    best: &Candidate,
) -> bool {
    let installed_at = record.release_id.as_deref().and_then(|id| {
        let in_set = entry
            .set
            .iter()
            .flat_map(|s| &s.releases)
            .find(|r| same_release(&r.release_id, id))
            .and_then(|r| r.published_at);
        let known = entry
            .known
            .iter()
            .find(|k| same_release(&k.release_id, id))
            .and_then(|k| k.published_at);
        in_set.or(known)
    });
    if let (Some(installed), Some(latest)) = (installed_at, newest.and_then(|r| r.published_at)) {
        return installed > latest;
    }
    compare_versions(&record.version, &best.version) == Some(Ordering::Greater)
}

/// The dotted numbers of a version: an optional leading `v`, then numbers separated by
/// dots; anything non-numeric after them (`-beta`, `+build`) is ignored. `None` when it
/// does not start with a number (`Plater-v662`, `bundle/v12.2.0`).
pub fn dotted(version: &str) -> Option<Vec<u64>> {
    let version = version.strip_prefix(['v', 'V']).unwrap_or(version);
    let mut numbers = Vec::new();
    for part in version.split('.') {
        let digits: String = part.chars().take_while(char::is_ascii_digit).collect();
        if digits.is_empty() {
            break;
        }
        numbers.push(digits.parse().ok()?);
        if digits.len() < part.len() {
            break;
        }
    }
    (!numbers.is_empty()).then_some(numbers)
}

/// `a` against `b` as dotted numbers (missing parts are zero); `None` when either does
/// not parse.
pub fn compare_versions(a: &str, b: &str) -> Option<Ordering> {
    let (a, b) = (dotted(a)?, dotted(b)?);
    let len = a.len().max(b.len());
    let part = |v: &[u64], i: usize| v.get(i).copied().unwrap_or(0);
    Some(
        (0..len)
            .map(|i| part(&a, i).cmp(&part(&b, i)))
            .find(|o| o.is_ne())
            .unwrap_or(Ordering::Equal),
    )
}

/// Whether the release tagged `installed` is older than `found`, a look-back's answer:
/// by their places in `order` (the feed's tags, newest first) when it holds both, else
/// by their dotted versions (a sibling probed outside the feed: `v3.217-mopc` before
/// `v3.218-mopc`). Unknown is never older, so a look-back never downgrades.
pub fn older_than_found(order: &[String], installed: &str, found: &str) -> bool {
    let at = |tag: &str| {
        order
            .iter()
            .position(|t| crate::sources::changelog::same_version(t, tag))
    };
    match (at(installed), at(found)) {
        (Some(installed), Some(found)) => installed > found,
        _ => compare_versions(installed, found) == Some(Ordering::Less),
    }
}

/// One addon's status on one target.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct StatusRow {
    /// The target's id.
    pub target: String,
    pub addon: String,
    #[serde(flatten)]
    pub status: UpdateStatus,
}

/// The status of every addon of every target, sorted by target id, then addon name
/// (ignoring case). `entry` looks a ref's memo entry up on a channel; `enabled` is the
/// set of sources that are on; `read_primary_interface` is called only for records
/// versioned by their TOC.
pub fn statuses(
    targets: &[(&Target, &TargetRecord)],
    entry: &dyn Fn(&Ref, Channel) -> Option<MemoEntry>,
    facts: &FactIndex,
    enabled: &BTreeSet<SourceId>,
    read_primary_interface: &dyn Fn(&Target, &AddonRecord) -> Option<u32>,
    look_back: &dyn Fn(&Ref, Channel, Family, &str) -> Option<LookBack>,
) -> Vec<StatusRow> {
    let mut rows = Vec::new();
    for (target, record) in targets {
        for addon in record.addons.values() {
            let installed = match addon.version_confidence {
                VersionConfidence::Toc => read_primary_interface(target, addon),
                VersionConfidence::Release => None,
            };
            let remembered = entry(&addon.r#ref, addon.channel);
            let source_enabled = enabled.contains(&SourceId::of(&addon.r#ref));
            // The newest release's tag (`gh:o/r@<tag>`) the look-back was made against.
            let looked_back = |newest: &str| {
                let tag = newest.rsplit_once('@').map(|(_, tag)| tag)?;
                look_back(&addon.r#ref, addon.channel, target.family, tag)
            };
            rows.push(StatusRow {
                target: target.id.clone(),
                addon: addon.name.clone(),
                status: evaluate(
                    addon,
                    remembered.as_ref(),
                    facts,
                    target,
                    installed,
                    source_enabled,
                    &looked_back,
                ),
            });
        }
    }
    rows.sort_by(|a, b| {
        a.target
            .cmp(&b.target)
            .then_with(|| a.addon.to_lowercase().cmp(&b.addon.to_lowercase()))
    });
    rows
}

/// What an update of `record` must not go below (`Update.installed_primary_interface`):
/// the installed primary interface of a record versioned by its TOC, else `None`.
pub fn update_floor(target: &Target, record: &AddonRecord, table: &RulesTable) -> Option<u32> {
    match record.version_confidence {
        VersionConfidence::Toc => installed_primary_interface(target, record, table),
        VersionConfidence::Release => None,
    }
}

/// The in-family interface of `record`'s installed primary folder, read from the TOC
/// the client loads, as the planner computes it for a candidate. The module's only I/O.
pub fn installed_primary_interface(
    target: &Target,
    record: &AddonRecord,
    table: &RulesTable,
) -> Option<u32> {
    let primary = crate::engine::plan::primary_folder(&record.r#ref, &record.folders, None)?;
    let view = DiskFolder::new(target.addons_dir().join(primary.as_str())).ok()?;
    let loaded = loaded_toc(&view, &target.rules).ok().flatten()?;
    primary_interface(
        &loaded,
        &target.rules,
        target.family,
        target.interface,
        table,
    )
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    use super::*;
    use crate::error::ErrorCode;
    use crate::model::{
        AddonReason, AddonState, Asset, AssetFlavor, AssetLocation, FolderName, ReleaseSet,
        TargetKey, TxId,
    };
    use crate::state::memo::{CandidateFact, KnownRelease, MemoFailure, MemoWanted, VerdictFact};

    const FOO: &str = "gh:o/foo";

    fn at(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    fn retail() -> Target {
        crate::engine::testing::target(std::path::Path::new("/nonexistent/_retail_"))
    }

    fn record(release: Option<&str>, version: &str) -> AddonRecord {
        AddonRecord {
            r#ref: FOO.parse().unwrap(),
            name: "Foo".into(),
            version: version.into(),
            version_confidence: VersionConfidence::Release,
            release_id: release.map(|tag| format!("{FOO}@{tag}")),
            channel: Channel::Stable,
            pinned: false,
            reason: AddonReason::Explicit,
            required_by: Vec::new(),
            folders: vec![FolderName::new("Foo").unwrap()],
            state: AddonState::Ok,
            installed_at: at("2026-10-01T00:00:00Z"),
            tx_id: TxId::new(),
            blob_sha256: None,
            revision: None,
            left_out: Vec::new(),
            any_flavor: false,
            skipped_releases: Vec::new(),
        }
    }

    /// An identified adoption: versioned by its TOC, no release recorded.
    fn adopted() -> AddonRecord {
        let mut r = record(None, "1.0");
        r.version_confidence = VersionConfidence::Toc;
        r
    }

    /// An asset ranked declared (`mainline` at retail's interface), likely (no metadata)
    /// or unlikely (another flavor only).
    fn asset(name: &str, kind: Confidence) -> Asset {
        let flavors = match kind {
            Confidence::Declared => vec![("mainline", 120_105)],
            Confidence::Unlikely => vec![("vanilla", 11_509)],
            _ => Vec::new(),
        };
        Asset {
            filename: name.into(),
            location: AssetLocation::Url(format!("https://github.com/o/foo/{name}")),
            size: None,
            expected_hash: None,
            flavors: flavors
                .into_iter()
                .map(|(flavor, interface)| AssetFlavor {
                    flavor: flavor.into(),
                    interface: Some(interface),
                    source: crate::model::FlavorSource::ReleaseJson,
                })
                .collect(),
            name_flavors: Vec::new(),
            nolib: false,
        }
    }

    fn release(
        tag: &str,
        channel: Channel,
        published: Option<&str>,
        assets: Vec<Asset>,
    ) -> Release {
        Release {
            release_id: format!("{FOO}@{tag}"),
            version: tag.into(),
            channel,
            published_at: published.map(at),
            assets,
            changelog_url: None,
            notes: None,
            name_hint: None,
            revision: None,
        }
    }

    fn entry(newest: Release, known: Vec<KnownRelease>) -> MemoEntry {
        MemoEntry {
            wanted: MemoWanted {
                r#ref: FOO.parse().unwrap(),
                channel: Channel::Stable,
            },
            fetched_at: at("2026-10-15T00:00:00Z"),
            probe: None,
            set: Some(ReleaseSet {
                r#ref: FOO.parse().unwrap(),
                releases: vec![newest],
            }),
            known,
            failure: None,
        }
    }

    /// Release 2.0.0 with one declared asset.
    fn newest() -> MemoEntry {
        let assets = vec![asset("Foo.zip", Confidence::Declared)];
        entry(release("2.0.0", Channel::Stable, None, assets), Vec::new())
    }

    /// A verdict on retail for `asset` of release `tag`.
    fn fact(tag: &str, asset: &str, loads: bool, out_of_date: bool, primary: u32) -> CandidateFact {
        CandidateFact {
            release_id: format!("{FOO}@{tag}"),
            asset: asset.into(),
            sha256: "aa".into(),
            size: 1,
            verdicts: vec![VerdictFact {
                rules: retail().rules.name,
                interface: 120_105,
                loads,
                out_of_date,
                primary_interface: Some(primary),
                at: at("2026-10-15T00:00:00Z"),
                judge: Some(retail().judge),
            }],
            unverified: false,
        }
    }

    fn eval(
        record: &AddonRecord,
        entry: Option<&MemoEntry>,
        facts: &[CandidateFact],
    ) -> UpdateStatus {
        evaluate(
            record,
            entry,
            &FactIndex::new(facts),
            &retail(),
            Some(120_105),
            true,
            &|_| None,
        )
    }

    fn name(status: &UpdateStatus) -> String {
        serde_json::to_value(status).unwrap()["status"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    fn confidence(status: &UpdateStatus) -> Option<Confidence> {
        match status {
            UpdateStatus::UpdateAvailable { confidence, .. } => Some(*confidence),
            _ => None,
        }
    }

    #[test]
    fn current_pinned_and_updates_by_confidence() {
        let e = newest();
        assert_eq!(
            name(&eval(&record(Some("2.0.0"), "2.0.0"), Some(&e), &[])),
            "current"
        );
        let mut pinned = record(Some("1.0.0"), "1.0.0");
        pinned.pinned = true;
        let status = eval(&pinned, Some(&e), &[]);
        assert!(
            matches!(&status, UpdateStatus::Pinned { latest: Some(l) } if l.version == "2.0.0"),
            "{status:?}"
        );
        pinned.release_id = Some(format!("{FOO}@2.0.0"));
        assert_eq!(
            name(&eval(&pinned, Some(&e), &[])),
            "current",
            "pinned at the newest"
        );
        // A record installed through another spelling of the repository is the same
        // release: the memo is shared by identity.
        let mut spelled = record(Some("2.0.0"), "2.0.0");
        spelled.release_id = Some("gh:O/Foo@2.0.0".into());
        assert_eq!(
            name(&eval(&spelled, Some(&e), &[])),
            "current",
            "{spelled:?}"
        );
        assert!(!same_release(
            &format!("{FOO}@2.0.0"),
            &format!("{FOO}@2.0.0-RC")
        ));

        let behind = record(Some("1.0.0"), "1.0.0");
        assert_eq!(
            confidence(&eval(&behind, Some(&e), &[])),
            Some(Confidence::Declared)
        );
        let verified = [fact("2.0.0", "Foo.zip", true, false, 120_105)];
        assert_eq!(
            confidence(&eval(&behind, Some(&e), &verified)),
            Some(Confidence::Verified)
        );
        for (kind, expected) in [
            (Confidence::Likely, Confidence::Likely),
            (Confidence::Unlikely, Confidence::Unlikely),
        ] {
            let e = entry(
                release("2.0.0", Channel::Stable, None, vec![asset("Foo.zip", kind)]),
                Vec::new(),
            );
            assert_eq!(confidence(&eval(&behind, Some(&e), &[])), Some(expected));
        }
    }

    /// The same release id at another revision is a re-tag, an update; at the same
    /// revision it is current, whatever else the repository did.
    #[test]
    fn a_re_tag_is_an_update_and_the_same_revision_is_current() {
        let mut e = newest();
        if let Some(set) = e.set.as_mut() {
            set.releases[0].revision = Some(403);
        }
        let mut installed = record(Some("2.0.0"), "2.0.0");
        installed.revision = Some(403);
        assert_eq!(name(&eval(&installed, Some(&e), &[])), "current");

        installed.revision = Some(401);
        let status = eval(&installed, Some(&e), &[]);
        assert_eq!(name(&status), "update_available", "{status:?}");
        let latest = status.latest().unwrap();
        assert_eq!(
            (latest.revision, latest.retagged, latest.shown().as_str()),
            (Some(403), true, "2.0.0 (re-tagged upstream)")
        );
        let json = serde_json::to_value(&status).unwrap();
        assert_eq!(
            (&json["latest"]["revision"], &json["latest"]["retagged"]),
            (&403.into(), &true.into())
        );

        let plain = eval(&record(Some("1.0.0"), "1.0.0"), Some(&newest()), &[]);
        let json = serde_json::to_value(&plain).unwrap();
        assert!(
            json["latest"].get("retagged").is_none() && json["latest"].get("revision").is_none(),
            "{json}"
        );
    }

    #[test]
    fn identified_adoptions_update_capped_at_likely_without_a_fact() {
        let e = newest();
        assert_eq!(
            confidence(&eval(&adopted(), Some(&e), &[])),
            Some(Confidence::Likely),
            "declared, capped"
        );
        let verified = [fact("2.0.0", "Foo.zip", true, false, 120_105)];
        assert_eq!(
            confidence(&eval(&adopted(), Some(&e), &verified)),
            Some(Confidence::Verified)
        );
    }

    #[test]
    fn never_a_downgrade() {
        // After a switch back to stable: the installed beta is newer than stable 2.0.0.
        let mut beta = record(Some("3.0.0-beta"), "3.0.0-beta");
        beta.channel = Channel::Stable;
        let dated = |installed: &str| {
            entry(
                release(
                    "2.0.0",
                    Channel::Stable,
                    Some("2026-10-01T00:00:00Z"),
                    vec![asset("Foo.zip", Confidence::Declared)],
                ),
                vec![KnownRelease {
                    release_id: format!("{FOO}@3.0.0-beta"),
                    published_at: Some(at(installed)),
                }],
            )
        };
        let status = eval(&beta, Some(&dated("2026-10-10T00:00:00Z")), &[]);
        assert_eq!(name(&status), "ahead", "by dates");
        let status = eval(&beta, Some(&dated("2026-09-01T00:00:00Z")), &[]);
        assert_eq!(
            name(&status),
            "update_available",
            "dates decide when both are known"
        );
        assert_eq!(
            name(&eval(&beta, Some(&newest()), &[])),
            "ahead",
            "by versions"
        );
        // `pinned` says a newer release exists: a pin ahead of the newest is `ahead`.
        beta.pinned = true;
        assert_eq!(
            name(&eval(&beta, Some(&newest()), &[])),
            "ahead",
            "pinned ahead"
        );
        beta.pinned = false;
        let mut odd = record(Some("nightly"), "nightly-build");
        odd.channel = Channel::Stable;
        let e = entry(
            release(
                "Plater-v662",
                Channel::Stable,
                None,
                vec![asset("P.zip", Confidence::Declared)],
            ),
            Vec::new(),
        );
        assert_eq!(
            name(&eval(&odd, Some(&e), &[])),
            "update_available",
            "neither comparison is possible"
        );
    }

    #[test]
    fn facts_block_what_does_not_suit_this_client() {
        let assets = vec![
            asset("Foo.zip", Confidence::Declared),
            asset("Foo-nolib.zip", Confidence::Likely),
        ];
        let e = entry(release("2.0.0", Channel::Stable, None, assets), Vec::new());
        let behind = record(Some("1.0.0"), "1.0.0");
        let both = [
            fact("2.0.0", "Foo.zip", false, false, 11_509),
            fact("2.0.0", "Foo-nolib.zip", false, false, 11_509),
        ];
        let status = eval(&behind, Some(&e), &both);
        assert!(
            matches!(
                status,
                UpdateStatus::Blocked {
                    reason: BlockedReason::VerifiedNotLoading,
                    ..
                }
            ),
            "{status:?}"
        );
        let one = [fact("2.0.0", "Foo.zip", false, false, 11_509)];
        assert_eq!(
            name(&eval(&behind, Some(&e), &one)),
            "update_available",
            "a candidate without a fact does not count as failing"
        );

        let older = |fact| eval(&adopted(), Some(&newest()), &[fact]);
        for f in [
            fact("2.0.0", "Foo.zip", true, false, 120_000),
            fact("2.0.0", "Foo.zip", true, true, 120_105),
        ] {
            let status = older(f);
            assert!(
                matches!(
                    status,
                    UpdateStatus::Blocked {
                        reason: BlockedReason::OlderThanInstalled,
                        ..
                    }
                ),
                "{status:?}"
            );
        }
    }

    /// A newest release proven not to load here, with a look-back remembered against it:
    /// the release found is this client's latest, an update only from older in the feed.
    #[test]
    fn a_look_back_names_this_clients_latest() {
        let e = newest();
        let refused = [fact("2.0.0", "Foo.zip", false, false, 11_509)];
        let found = LookBack {
            found: Some(ReleaseSet {
                r#ref: FOO.parse().unwrap(),
                releases: vec![release(
                    "1.5.0",
                    Channel::Stable,
                    None,
                    vec![asset("Foo.zip", Confidence::Declared)],
                )],
            }),
            ruled_out: Vec::new(),
            order: ["2.0.0", "1.5.0", "1.0.0"].map(String::from).to_vec(),
        };
        let with = |installed: &AddonRecord, look_back: Option<&LookBack>| {
            let seen = std::cell::RefCell::new(None);
            let status = evaluate(
                installed,
                Some(&e),
                &FactIndex::new(&refused),
                &retail(),
                Some(120_105),
                true,
                &|newest| {
                    seen.replace(Some(newest.to_owned()));
                    look_back.cloned()
                },
            );
            (status, seen.into_inner())
        };
        let (status, asked) = with(&record(Some("1.0.0"), "1.0.0"), Some(&found));
        assert_eq!(asked.as_deref(), Some(format!("{FOO}@2.0.0").as_str()));
        assert!(
            matches!(&status, UpdateStatus::UpdateAvailable { latest, confidence: Confidence::Declared } if latest.version == "1.5.0"),
            "{status:?}"
        );
        let (status, _) = with(&record(Some("1.5.0"), "1.5.0"), Some(&found));
        assert!(
            matches!(&status, UpdateStatus::Current { latest } if latest.version == "1.5.0"),
            "{status:?}"
        );
        // Out of the feed's reach: placed by version when it has one, else never moved.
        let (status, _) = with(&record(Some("0.9.0"), "0.9.0"), Some(&found));
        assert_eq!(name(&status), "update_available", "{status:?}");
        let (status, _) = with(&record(Some("2.5.0"), "2.5.0"), Some(&found));
        assert_eq!(
            name(&status),
            "ahead",
            "newer than the newest itself: {status:?}"
        );
        let (status, _) = with(&record(Some("hotfix"), "hotfix"), Some(&found));
        assert_eq!(name(&status), "current", "unordered: {status:?}");
        // Nothing found, or no look-back: blocked as before.
        let none = LookBack {
            found: None,
            ..found.clone()
        };
        for look_back in [Some(&none), None] {
            let (status, _) = with(&record(Some("1.0.0"), "1.0.0"), look_back);
            assert!(
                matches!(
                    status,
                    UpdateStatus::Blocked {
                        reason: BlockedReason::VerifiedNotLoading,
                        ..
                    }
                ),
                "{status:?}"
            );
        }
    }

    /// A status names the file of the candidate it was judged on: the best-ranked one,
    /// not the release's first asset; for a looked-back release, the found one's.
    #[test]
    fn brief_keeps_the_best_candidates_asset() {
        let behind = record(Some("1.0.0"), "1.0.0");
        let e = entry(
            release(
                "2.0.0",
                Channel::Stable,
                None,
                vec![
                    asset("Foo-vanilla.zip", Confidence::Unlikely),
                    asset("Foo-mainline.zip", Confidence::Declared),
                ],
            ),
            Vec::new(),
        );
        let status = eval(&behind, Some(&e), &[]);
        assert_eq!(
            (name(&status), status.latest().and_then(|l| l.asset.clone())),
            (
                "update_available".to_owned(),
                Some("Foo-mainline.zip".into())
            ),
            "{status:?}"
        );

        let refused = [fact("2.0.0", "Foo.zip", false, false, 11_509)];
        let found = LookBack {
            found: Some(ReleaseSet {
                r#ref: FOO.parse().unwrap(),
                releases: vec![release(
                    "1.5.0",
                    Channel::Stable,
                    None,
                    vec![asset("Foo-1.5.zip", Confidence::Declared)],
                )],
            }),
            ruled_out: Vec::new(),
            order: ["2.0.0", "1.5.0", "1.0.0"].map(String::from).to_vec(),
        };
        let status = evaluate(
            &behind,
            Some(&newest()),
            &FactIndex::new(&refused),
            &retail(),
            Some(120_105),
            true,
            &|_| Some(found.clone()),
        );
        assert_eq!(
            (
                status.latest().map(|l| l.version.clone()),
                status.latest().and_then(|l| l.asset.clone())
            ),
            (Some("1.5.0".into()), Some("Foo-1.5.zip".into())),
            "looked back: {status:?}"
        );
    }

    /// The asset is for `list`'s update entry only: a brief serializes the keys it
    /// always had, so `outdated`'s and `info`'s wire is unchanged.
    #[test]
    fn release_brief_serializes_without_its_asset() {
        for (case, revision, retagged, want) in [
            (
                "plain",
                None,
                false,
                vec!["channel", "published_at", "release_id", "version"],
            ),
            (
                "re-tagged",
                Some(404),
                true,
                vec![
                    "channel",
                    "published_at",
                    "release_id",
                    "retagged",
                    "revision",
                    "version",
                ],
            ),
        ] {
            let brief = ReleaseBrief {
                version: "2.0.0".into(),
                release_id: format!("{FOO}@2.0.0"),
                published_at: None,
                channel: Channel::Stable,
                revision,
                retagged,
                asset: Some("Foo.zip".into()),
            };
            let value = serde_json::to_value(&brief).unwrap();
            let mut keys: Vec<&str> = value
                .as_object()
                .map(|o| o.keys().map(String::as_str).collect())
                .unwrap_or_default();
            keys.sort_unstable();
            assert_eq!(keys, want, "{case}: {value}");
        }
    }

    #[test]
    fn static_unchecked_and_failed() {
        let e = newest();
        for r in [
            "file:/home/me/Foo.zip",
            "file:/home/me/Foo",
            "url:https://github.com/o/foo/releases/download/v1/Foo.zip",
        ] {
            let mut local = record(Some("x"), "1");
            local.r#ref = r.parse().unwrap();
            assert_eq!(eval(&local, Some(&e), &[]), UpdateStatus::Static, "{r}");
        }
        for id in [
            "sha256:abcd",
            "listing:ab12",
            "dir:01JB8Q2VA00000000000000000",
        ] {
            let given = record(Some(id), "1");
            assert_eq!(eval(&given, Some(&e), &[]), UpdateStatus::Static, "{id}");
        }
        let behind = record(Some("1.0.0"), "1.0.0");
        assert_eq!(name(&eval(&behind, None, &[])), "unchecked");
        let off = evaluate(
            &behind,
            Some(&e),
            &FactIndex::default(),
            &retail(),
            None,
            false,
            &|_| None,
        );
        assert_eq!(
            off,
            UpdateStatus::Unchecked {
                reason: UncheckedReason::SourceDisabled
            }
        );
        let beta_only = entry(
            release(
                "3.0.0",
                Channel::Beta,
                None,
                vec![asset("Foo.zip", Confidence::Declared)],
            ),
            Vec::new(),
        );
        assert_eq!(
            eval(&behind, Some(&beta_only), &[]),
            UpdateStatus::Unchecked {
                reason: UncheckedReason::NoEligibleRelease
            }
        );

        let mut gone = newest();
        gone.set = None;
        gone.failure = Some(MemoFailure {
            code: ErrorCode::NotFound,
            message: "gh:o/foo has no releases".into(),
            at: at("2026-10-15T00:00:00Z"),
        });
        let status = eval(&behind, Some(&gone), &[]);
        let json = serde_json::to_value(&status).unwrap();
        assert_eq!(
            (
                &json["status"],
                &json["error"]["code"],
                &json["error"]["fix_argv"]
            ),
            (
                &serde_json::json!("failed"),
                &serde_json::json!("not_found"),
                &serde_json::json!(["omaddons", "remove", "Foo", "-t", "retail"])
            )
        );
        // It may have moved: adopting its folder under the new ref re-points it.
        assert_eq!(
            (
                &json["error"]["alternatives"][0]["display"],
                &json["error"]["alternatives"][0]["argv"]
            ),
            (
                &serde_json::json!(
                    "if it moved, re-point it: omaddons adopt Foo --ref <new ref> -t retail"
                ),
                &serde_json::Value::Null
            )
        );
    }

    #[test]
    fn dotted_versions_compare_by_number() {
        assert_eq!(
            compare_versions("v12.1.15", "12.1.14"),
            Some(Ordering::Greater)
        );
        assert_eq!(
            compare_versions("12.1.14-beta", "12.1.14"),
            Some(Ordering::Equal)
        );
        assert_eq!(compare_versions("12.1", "12.1.0"), Some(Ordering::Equal));
        assert_eq!(compare_versions("12.10", "12.9"), Some(Ordering::Greater));
        assert_eq!(compare_versions("Plater-v662", "662"), None);
        assert_eq!(compare_versions("bundle/v12.2.0+v1.0.7", "12.2.0"), None);
        assert_eq!(dotted("v5.19.2+build.7"), Some(vec![5, 19, 2]));
    }

    #[test]
    fn statuses_are_sorted_and_read_installed_toc_only_for_adoptions() {
        let mut era = retail();
        era.id = "era".into();
        let record_of = |addons: Vec<AddonRecord>| {
            let mut r = TargetRecord::empty(TargetKey::new());
            for a in addons {
                r.addons.insert(TargetRecord::addon_key(&a.name), a);
            }
            r
        };
        let named = |name: &str, mut r: AddonRecord| {
            r.name = name.into();
            r
        };
        let retail_record = record_of(vec![
            named("zeta", record(Some("1.0.0"), "1.0.0")),
            named("Alpha", adopted()),
        ]);
        let era_record = record_of(vec![named("Mid", record(Some("2.0.0"), "2.0.0"))]);
        let retail_target = retail();
        let targets = [(&retail_target, &retail_record), (&era, &era_record)];
        let memo = newest();
        let read = RefCell::new(Vec::new());
        let enabled: BTreeSet<SourceId> = [SourceId::Github].into();
        let rows = statuses(
            &targets,
            &|_, _| Some(memo.clone()),
            &FactIndex::default(),
            &enabled,
            &|_, addon| {
                read.borrow_mut().push(addon.name.clone());
                Some(120_105)
            },
            &|_, _, _, _| None,
        );
        let order: Vec<(&str, &str)> = rows
            .iter()
            .map(|r| (r.target.as_str(), r.addon.as_str()))
            .collect();
        assert_eq!(
            order,
            [("era", "Mid"), ("retail", "Alpha"), ("retail", "zeta")]
        );
        assert_eq!(*read.borrow(), ["Alpha"], "only the TOC-versioned record");
        let json = serde_json::to_value(&rows[0]).unwrap();
        assert_eq!(
            (&json["target"], &json["addon"], &json["status"]),
            (
                &serde_json::json!("era"),
                &serde_json::json!("Mid"),
                &serde_json::json!("current")
            ),
            "flattened"
        );
    }

    #[test]
    fn every_variant_serializes_as_documented() {
        let latest = || ReleaseBrief {
            version: "2.0.0".into(),
            release_id: format!("{FOO}@2.0.0"),
            published_at: Some(at("2026-10-01T00:00:00Z")),
            channel: Channel::Stable,
            revision: None,
            retagged: false,
            asset: Some("Foo.zip".into()),
        };
        let failed = Error::new(
            ErrorCode::NotFound,
            "gh:o/foo has no releases",
            Suggestion::command(["omaddons", "remove", "Foo", "-t", "retail"]),
        );
        let variants: BTreeMap<&str, UpdateStatus> = BTreeMap::from([
            ("current", UpdateStatus::Current { latest: latest() }),
            (
                "update_available",
                UpdateStatus::UpdateAvailable {
                    latest: latest(),
                    confidence: Confidence::Declared,
                },
            ),
            (
                "pinned",
                UpdateStatus::Pinned {
                    latest: Some(latest()),
                },
            ),
            (
                "skipped_release",
                UpdateStatus::SkippedRelease {
                    latest: latest(),
                    skipped_release: SkippedRelease {
                        release: ReleaseKey::parse(&format!("{FOO}@2.0.0"), None),
                        version: "2.0.0".into(),
                        at: at("2026-11-01T00:00:00Z"),
                        origin: crate::model::SkipOrigin::Undo,
                        tx: "01JB8Q2VA00000000000000009".parse().unwrap(),
                    },
                },
            ),
            ("ahead", UpdateStatus::Ahead { latest: latest() }),
            (
                "blocked",
                UpdateStatus::Blocked {
                    latest: latest(),
                    reason: BlockedReason::OlderThanInstalled,
                },
            ),
            (
                "failed",
                UpdateStatus::Failed {
                    error: ErrorBody::from(failed),
                },
            ),
            (
                "unchecked",
                UpdateStatus::Unchecked {
                    reason: UncheckedReason::NeverChecked,
                },
            ),
            ("static", UpdateStatus::Static),
        ]);
        insta::assert_json_snapshot!("update_statuses", variants);
    }

    // ---- Skip marks ------------------------------------------------------------------

    /// An exact mark on `release_id` at `revision`, or a label mark when `release_id` is
    /// `None`, labeled `version`.
    fn mark(release_id: Option<&str>, revision: Option<u32>, version: &str) -> SkippedRelease {
        SkippedRelease {
            release: release_id.map(|id| ReleaseKey::parse(id, revision).unwrap()),
            version: version.into(),
            at: at("2026-11-01T00:00:00Z"),
            origin: crate::model::SkipOrigin::Undo,
            tx: TxId::new(),
        }
    }

    /// `evaluate`'s order with a mark beside each neighboring state (design 7.5.2): a
    /// mark holds the release that would be offered, the look-back's included, after the
    /// pin and before `blocked`; a release that is current or ahead offers nothing.
    #[test]
    fn a_mark_holds_the_release_that_would_be_offered() {
        let foo = |tag: &str| format!("{FOO}@{tag}");
        let newest_rev = |revision: u32| {
            let mut e = newest();
            if let Some(set) = e.set.as_mut() {
                set.releases[0].revision = Some(revision);
            }
            e
        };
        let found = LookBack {
            found: Some(ReleaseSet {
                r#ref: FOO.parse().unwrap(),
                releases: vec![release(
                    "1.5.0",
                    Channel::Stable,
                    None,
                    vec![asset("Foo.zip", Confidence::Declared)],
                )],
            }),
            ruled_out: Vec::new(),
            order: ["2.0.0", "1.5.0", "1.0.0"].map(String::from).to_vec(),
        };
        let refused = vec![fact("2.0.0", "Foo.zip", false, false, 11_509)];
        let older = vec![fact("2.0.0", "Foo.zip", true, true, 120_105)];
        let with_marks = |mut r: AddonRecord, marks: Vec<SkippedRelease>| {
            r.skipped_releases = marks;
            r
        };
        let pinned = |mut r: AddonRecord| {
            r.pinned = true;
            r
        };
        let at_revision = |mut r: AddonRecord, revision: u32| {
            r.revision = Some(revision);
            r
        };
        // Release 1 marked, then nine later ones each marked in turn: the first holds.
        let ten: Vec<SkippedRelease> = std::iter::once(mark(Some(&foo("2.0.0")), None, "2.0.0"))
            .chain((1..10).map(|n| mark(Some(&foo(&format!("2.0.{n}"))), None, "x")))
            .collect();
        let behind = || record(Some("1.0.0"), "1.0.0");
        // (case, the record, the memo entry, the facts, the look-back's answer, the
        // status, the version it names)
        type Case<'a> = (
            &'a str,
            AddonRecord,
            MemoEntry,
            Vec<CandidateFact>,
            Option<&'a LookBack>,
            &'a str,
            Option<&'a str>,
        );
        let cases: Vec<Case<'_>> = vec![
            (
                "a mark on the newest",
                with_marks(behind(), vec![mark(Some(&foo("2.0.0")), None, "2.0.0")]),
                newest(),
                Vec::new(),
                None,
                "skipped_release",
                Some("2.0.0"),
            ),
            (
                "a label mark on the newest",
                with_marks(behind(), vec![mark(None, None, "v2.0.0")]),
                newest(),
                Vec::new(),
                None,
                "skipped_release",
                Some("2.0.0"),
            ),
            (
                "a mark on another release",
                with_marks(behind(), vec![mark(Some(&foo("1.5.0")), None, "1.5.0")]),
                newest(),
                Vec::new(),
                None,
                "update_available",
                Some("2.0.0"),
            ),
            (
                "a mark holds however many came after it",
                with_marks(behind(), ten),
                newest(),
                Vec::new(),
                None,
                "skipped_release",
                Some("2.0.0"),
            ),
            (
                "pinned first",
                pinned(with_marks(
                    behind(),
                    vec![mark(Some(&foo("2.0.0")), None, "2.0.0")],
                )),
                newest(),
                Vec::new(),
                None,
                "pinned",
                Some("2.0.0"),
            ),
            (
                "the newest installed is current",
                with_marks(
                    record(Some("2.0.0"), "2.0.0"),
                    vec![mark(None, None, "2.0.0")],
                ),
                newest(),
                Vec::new(),
                None,
                "current",
                Some("2.0.0"),
            ),
            (
                "ahead offers nothing",
                with_marks(
                    record(Some("3.0.0-beta"), "3.0.0-beta"),
                    vec![mark(Some(&foo("2.0.0")), None, "2.0.0")],
                ),
                newest(),
                Vec::new(),
                None,
                "ahead",
                Some("2.0.0"),
            ),
            (
                "before blocked: not loading",
                with_marks(behind(), vec![mark(Some(&foo("2.0.0")), None, "2.0.0")]),
                newest(),
                refused.clone(),
                None,
                "skipped_release",
                Some("2.0.0"),
            ),
            (
                "blocked without a mark",
                behind(),
                newest(),
                refused.clone(),
                None,
                "blocked",
                Some("2.0.0"),
            ),
            (
                "before blocked: older than installed",
                with_marks(adopted(), vec![mark(Some(&foo("2.0.0")), None, "2.0.0")]),
                newest(),
                older,
                None,
                "skipped_release",
                Some("2.0.0"),
            ),
            (
                "the look-back's release, marked",
                with_marks(behind(), vec![mark(Some(&foo("1.5.0")), None, "1.5.0")]),
                newest(),
                refused.clone(),
                Some(&found),
                "skipped_release",
                Some("1.5.0"),
            ),
            (
                "the look-back offers its own, the newest's mark aside",
                with_marks(behind(), vec![mark(Some(&foo("2.0.0")), None, "2.0.0")]),
                newest(),
                refused.clone(),
                Some(&found),
                "update_available",
                Some("1.5.0"),
            ),
            (
                "the look-back finds it current",
                with_marks(
                    record(Some("1.5.0"), "1.5.0"),
                    vec![mark(Some(&foo("2.0.0")), None, "2.0.0")],
                ),
                newest(),
                refused,
                Some(&found),
                "current",
                Some("1.5.0"),
            ),
            (
                "a re-tag at the marked revision",
                with_marks(
                    at_revision(record(Some("2.0.0"), "2.0.0"), 401),
                    vec![mark(Some(&foo("2.0.0")), Some(403), "2.0.0")],
                ),
                newest_rev(403),
                Vec::new(),
                None,
                "skipped_release",
                Some("2.0.0"),
            ),
            (
                "a re-tag past the marked revision is a new release",
                with_marks(
                    at_revision(record(Some("1.0.0"), "1.0.0"), 1),
                    vec![mark(Some(&foo("2.0.0")), Some(1390), "2.0.0")],
                ),
                newest_rev(1402),
                Vec::new(),
                None,
                "update_available",
                Some("2.0.0"),
            ),
        ];
        for (case, installed, entry, facts, look_back, expected, version) in cases {
            let status = evaluate(
                &installed,
                Some(&entry),
                &FactIndex::new(&facts),
                &retail(),
                Some(120_105),
                true,
                &|_| look_back.cloned(),
            );
            assert_eq!(
                (
                    name(&status).as_str(),
                    status.latest().map(|l| l.version.as_str())
                ),
                (expected, version),
                "{case}: {status:?}"
            );
            if let UpdateStatus::SkippedRelease {
                latest,
                skipped_release,
            } = &status
            {
                assert_eq!(
                    covering(&installed, latest),
                    Some(skipped_release),
                    "{case}: the mark named is the one covering the release"
                );
            }
        }
    }

    #[test]
    fn skip_covering_matches_exact_and_label_marks() {
        // (case, the mark, the release's id and revision (no id: a keyless release), its
        // version, whether the mark covers it)
        let cases = [
            (
                "an exact mark covers an equal key in another spelling of the ref",
                mark(Some("gh:WeakAuras/WeakAuras2@5.19.2"), None, "5.19.2"),
                Some(("gh:weakauras/WEAKAURAS2@5.19.2", None)),
                "5.19.2",
                true,
            ),
            (
                "an exact mark does not cover another tag",
                mark(Some("gh:o/foo@v2"), None, "v2"),
                Some(("gh:o/foo@v3", None)),
                "v3",
                false,
            ),
            (
                "a WowAce mark at revision 1390 does not cover revision 1402 of its tag",
                mark(Some("wowace:bug-grabber@v12.1.1"), Some(1390), "v12.1.1"),
                Some(("wowace:bug-grabber@v12.1.1", Some(1402))),
                "v12.1.1",
                false,
            ),
            (
                "a WowAce mark at revision 1390 covers revision 1390",
                mark(Some("wowace:bug-grabber@v12.1.1"), Some(1390), "v12.1.1"),
                Some(("wowace:bug-grabber@v12.1.1", Some(1390))),
                "v12.1.1",
                true,
            ),
            (
                "a mark with a revision does not cover the tag at an unknown one",
                mark(Some("wowace:bug-grabber@v12.1.1"), Some(1390), "v12.1.1"),
                Some(("wowace:bug-grabber@v12.1.1", None)),
                "v12.1.1",
                false,
            ),
            (
                "a mark without a revision covers every revision of its tag",
                mark(Some("wowace:bug-grabber@v12.1.1"), None, "v12.1.1"),
                Some(("wowace:bug-grabber@v12.1.1", Some(1402))),
                "v12.1.1",
                true,
            ),
            (
                "a mark without a revision covers its tag without one",
                mark(Some("wowace:bug-grabber@v12.1.1"), None, "v12.1.1"),
                Some(("wowace:bug-grabber@v12.1.1", None)),
                "v12.1.1",
                true,
            ),
            (
                "a label mark covers its version with a leading v",
                mark(None, None, "2.0"),
                Some(("gh:o/foo@v2.0", None)),
                "v2.0",
                true,
            ),
            (
                "a label mark with a leading v covers its version without one",
                mark(None, None, "v2.0"),
                Some(("gh:o/bar@2.0", None)),
                "2.0",
                true,
            ),
            (
                "a label mark covers a keyless release by version",
                mark(None, None, "2.0"),
                None,
                "2.0",
                true,
            ),
            (
                "a label mark does not cover another version",
                mark(None, None, "2.0"),
                Some(("gh:o/foo@2.1", None)),
                "2.1",
                false,
            ),
            (
                "an exact mark is not widened by an equal label on another tag",
                mark(Some("gh:o/foo@v2"), None, "2.0"),
                Some(("gh:o/foo@release-2", None)),
                "2.0",
                false,
            ),
            (
                "an exact mark covers no keyless release",
                mark(Some("gh:o/foo@v2"), None, "v2"),
                None,
                "v2",
                false,
            ),
        ];
        for (case, marked, release, version, covers) in cases {
            let key = release.map(|(id, revision)| ReleaseKey::parse(id, revision).unwrap());
            assert_eq!(
                skip_covers(&marked, key.as_ref(), version),
                covers,
                "{case}: {marked:?} against {key:?} {version}"
            );
            let mut on = record(Some("v1"), "v1");
            on.skipped_releases = vec![mark(None, None, "never"), marked.clone()];
            assert_eq!(
                skip_covering(&on, key.as_ref(), version),
                covers.then_some(&marked),
                "{case}: skip_covering finds the covering mark"
            );
        }
    }
}
