//! The update status: one pure answer to "is there an update for this addon on this
//! client?", shared by `outdated`, `update`, `unpin`'s hint, `status` counts and `list`
//! (M2-D1, M2-D3). It reads the record, the release memo's entry, the candidate facts
//! and the target, and nothing else: no I/O and no clock, so `status --cached` can run
//! it on every refresh and every edge case is a table test.
//!
//! The order of the checks matters: a static install has nothing to check; a source
//! that is off, a missing entry or a remembered failure stop early; the installed
//! release being the newest is `current` even when pinned; a pin holds before anything
//! is compared; the downgrade guard runs before facts; and every confidence counts as
//! an update, because trying it and recording the verdict corrects the badge after at
//! most one attempt.

use std::cmp::Ordering;
use std::collections::BTreeSet;

use jiff::Timestamp;
use schemars::JsonSchema;
use serde::Serialize;

use crate::error::{Error, ErrorBody, Suggestion};
use crate::model::{
    AddonRecord, Candidate, Channel, RankReason, Ref, Release, Target, VersionConfidence,
};
use crate::resolve::select::select;
use crate::rules::RulesTable;
use crate::sources::SourceId;
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
    #[schemars(with = "Option<String>")]
    pub published_at: Option<Timestamp>,
    pub channel: Channel,
}

/// The name of a status, as the wire spells it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StatusKind {
    Current,
    UpdateAvailable,
    Pinned,
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
            | Self::Ahead { latest }
            | Self::Blocked { latest, .. } => Some(latest),
            Self::Pinned { latest } => latest.as_ref(),
            Self::Failed { .. } | Self::Unchecked { .. } | Self::Static => None,
        }
    }
}

/// Whether `record` has nothing upstream to compare (M2-D31): a `file:` or `url:` ref,
/// or a release id naming a given archive or directory (`@sha256:`, `@dir:`).
pub fn is_static(record: &AddonRecord) -> bool {
    let from_file = record
        .release_id
        .as_deref()
        .is_some_and(|id| id.contains("@sha256:") || id.contains("@dir:"));
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
    let latest = brief(best, newest);
    // 4. Installed already.
    if record.release_id.as_deref() == Some(best.release_id.as_str()) {
        return UpdateStatus::Current { latest };
    }
    // 5. A pin holds.
    if record.pinned {
        return UpdateStatus::Pinned {
            latest: Some(latest),
        };
    }
    // 6. Never a downgrade on its own.
    if installed_is_newer(record, entry, newest, best) {
        return UpdateStatus::Ahead { latest };
    }
    // 7. What plans learned.
    let rules = target.rules.name.as_str();
    let verdict =
        |c: &Candidate| facts.verdict(&c.release_id, &c.asset.filename, rules, target.interface);
    let verdicts: Vec<_> = candidates.iter().map(verdict).collect();
    if verdicts.iter().all(|v| v.is_some_and(|v| !v.loads)) {
        return UpdateStatus::Blocked {
            latest,
            reason: BlockedReason::VerifiedNotLoading,
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
    // 8. An update, and how likely it is to load.
    let confidence = match (loading, rank_confidence(best.rank)) {
        (Some(_), _) => Confidence::Verified,
        // Installed with only a TOC version: metadata is no more than likely.
        (None, Confidence::Declared) if by_toc => Confidence::Likely,
        (None, confidence) => confidence,
    };
    UpdateStatus::UpdateAvailable { latest, confidence }
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
    }
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
            .find(|r| r.release_id == id)
            .and_then(|r| r.published_at);
        let known = entry
            .known
            .iter()
            .find(|k| k.release_id == id)
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
        for id in ["sha256:abcd", "dir:01JB8Q2VA00000000000000000"] {
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
}
