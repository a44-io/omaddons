//! Candidate selection (vision §5.4, D28): from one fetched [`ReleaseSet`], the ordered
//! assets to try on one target. Pure and deterministic: one fetch per ref, one
//! selection per target.
//!
//! Selection only orders. The engine downloads, inspects and verifies each candidate
//! in turn; the first that verifies wins. Metadata is a hint, never a verdict (D5):
//! WeakAuras' `release.json` lists no `mainline` entry although its zip runs on retail,
//! so a metadata mismatch is ranked last, not dropped.
//!
//! The release: a pin (`--version` or a release URL's tag) picks it, `v` tolerated;
//! otherwise the newest release the channel admits. Only that release's assets become
//! candidates: silently falling back to an older version would mask a broken release.
//! The assets, best first: (1) metadata for the target's flavor (exact interface,
//! then the same `major.minor`), (2) a name hint for it, (3) unannotated, (4) name hints
//! for other flavors, (5) metadata for other flavors; `nolib: false` first in each tier.

use schemars::JsonSchema;
use serde::Serialize;

use crate::model::{
    Asset, Candidate, Channel, FlavorHint, RankReason, Release, ReleaseSet, Target,
};
use crate::rules::Family;

/// The candidates, or why there are none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    pub candidates: Vec<Candidate>,
    pub empty_reason: Option<EmptyReason>,
}

/// Why a selection is empty.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EmptyReason {
    /// No release on the requested channel.
    NoEligibleRelease,
    PinnedVersionNotFound,
    /// The chosen release has no installable asset.
    NoAssets,
}

/// The ordered candidates of `set` for `target` on `channel`, or of the release `pin`
/// names.
pub fn select(set: &ReleaseSet, target: &Target, channel: Channel, pin: Option<&str>) -> Selection {
    let empty = |reason| Selection {
        candidates: Vec::new(),
        empty_reason: Some(reason),
    };
    let release = match pin {
        Some(pin) => match set.releases.iter().find(|r| pinned(r, pin)) {
            Some(release) => release,
            None => return empty(EmptyReason::PinnedVersionNotFound),
        },
        None => match newest(set, channel) {
            Some(release) => release,
            None => return empty(EmptyReason::NoEligibleRelease),
        },
    };
    if release.assets.is_empty() {
        return empty(EmptyReason::NoAssets);
    }
    let mut ranked: Vec<(Tier, usize, &Asset)> = release
        .assets
        .iter()
        .enumerate()
        .map(|(i, asset)| (tier(asset, target), i, asset))
        .collect();
    ranked.sort_by_key(|(tier, i, asset)| (tier.order, asset.nolib, tier.closeness, *i));
    Selection {
        candidates: ranked
            .into_iter()
            .map(|(tier, _, asset)| Candidate {
                release_id: release.release_id.clone(),
                version: release.version.clone(),
                channel: release.channel,
                asset: asset.clone(),
                rank: tier.reason,
            })
            .collect(),
        empty_reason: None,
    }
}

/// Whether `release` is the one `pin` names: its version or tag (after the last `@` of
/// the release id), ignoring a leading `v` on either side.
fn pinned(release: &Release, pin: &str) -> bool {
    let bare = |s: &str| s.strip_prefix(['v', 'V']).unwrap_or(s).to_owned();
    let pin = bare(pin);
    let tag = release.release_id.rsplit_once('@').map(|(_, tag)| tag);
    [Some(release.version.as_str()), tag]
        .into_iter()
        .flatten()
        .any(|v| bare(v) == pin)
}

/// The newest release `channel` admits: by `published_at` when every eligible release
/// has one, else the first in set order (sources list newest first).
pub fn newest(set: &ReleaseSet, channel: Channel) -> Option<&Release> {
    let eligible: Vec<&Release> = set
        .releases
        .iter()
        .filter(|r| channel.includes(r.channel))
        .collect();
    if eligible.iter().all(|r| r.published_at.is_some()) {
        let mut best: Option<&Release> = None;
        for release in eligible {
            if best.is_none_or(|b| release.published_at > b.published_at) {
                best = Some(release);
            }
        }
        best
    } else {
        eligible.first().copied()
    }
}

/// An asset's place: tier first, then how close its interface is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Tier {
    order: u8,
    /// Within the metadata tier: 0 exact interface, 1 same `major.minor`, 2 other.
    closeness: u8,
    reason: RankReason,
}

fn tier(asset: &Asset, target: &Target) -> Tier {
    let make = |order, closeness, reason| Tier {
        order,
        closeness,
        reason,
    };
    if !asset.flavors.is_empty() {
        let matching: Vec<Option<u32>> = asset
            .flavors
            .iter()
            .filter(|f| Family::from_release_flavor(&f.flavor) == Some(target.family))
            .map(|f| f.interface)
            .collect();
        if matching.is_empty() || target.family.release_flavor().is_none() {
            return make(5, 0, RankReason::MetadataMismatch);
        }
        if matching.contains(&Some(target.interface)) {
            return make(1, 0, RankReason::MetadataInterface);
        }
        let near = matching
            .iter()
            .flatten()
            .any(|i| i / 100 == target.interface / 100);
        return make(1, if near { 1 } else { 2 }, RankReason::MetadataFlavor);
    }
    if asset.name_flavors.is_empty() {
        return make(3, 0, RankReason::Unannotated);
    }
    if asset
        .name_flavors
        .iter()
        .any(|h| hint_matches(*h, target.family))
    {
        make(2, 0, RankReason::NameHint)
    } else {
        make(4, 0, RankReason::NameHintMismatch)
    }
}

/// Whether a file-name flavor guess fits `family`; `ClassicAny` fits every modern
/// classic family. Legacy families and `unknown` match no hint.
fn hint_matches(hint: FlavorHint, family: Family) -> bool {
    let Some(flavor) = family.release_flavor() else {
        return false;
    };
    let named = match hint {
        FlavorHint::Mainline => "mainline",
        FlavorHint::Classic => "classic",
        FlavorHint::Bcc => "bcc",
        FlavorHint::Wrath => "wrath",
        FlavorHint::Cata => "cata",
        FlavorHint::Mists => "mists",
        FlavorHint::Titan => "titan",
        FlavorHint::Forever => "forever",
        FlavorHint::ClassicAny => return flavor != "mainline",
    };
    named == flavor
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;
    use crate::model::{AssetFlavor, AssetLocation};
    use crate::rules::{ClientRules, ClientVersion, InstallKind, RulesTable, TargetKind};

    fn target(version: &str, kind: InstallKind) -> Target {
        let table = RulesTable::builtin();
        let version: ClientVersion = version.parse().unwrap();
        let family = table.family_for(&version, kind);
        let interface =
            crate::rules::interface_for(&version, table.rules_for_family(family, &version, kind));
        let mut target = Target {
            key: None,
            id: format!("{family:?}"),
            install_root: "/games".into(),
            client_dir: "/games/x".into(),
            flavor_dir: None,
            product: None,
            install_kind: kind,
            family,
            kind: TargetKind::Live,
            version,
            interface,
            exe_name: None,
            label: None,
            rules: ClientRules::default(),
        };
        target.refresh_rules(&table);
        target
    }

    fn asset(
        name: &str,
        flavors: &[(&str, Option<u32>)],
        hints: &[FlavorHint],
        nolib: bool,
    ) -> Asset {
        Asset {
            filename: name.into(),
            location: AssetLocation::Url(format!("https://example.org/{name}")),
            size: None,
            expected_hash: None,
            flavors: flavors
                .iter()
                .map(|(flavor, interface)| AssetFlavor {
                    flavor: (*flavor).into(),
                    interface: *interface,
                })
                .collect(),
            name_flavors: hints.to_vec(),
            nolib,
        }
    }

    fn release(version: &str, channel: Channel, assets: Vec<Asset>) -> Release {
        Release {
            release_id: format!("gh:o/r@{version}"),
            version: version.into(),
            channel,
            published_at: None,
            assets,
            changelog_url: None,
        }
    }

    fn set(releases: Vec<Release>) -> ReleaseSet {
        ReleaseSet {
            r#ref: "gh:o/r".parse().unwrap(),
            releases,
        }
    }

    fn order(selection: &Selection) -> Vec<&str> {
        selection
            .candidates
            .iter()
            .map(|c| c.asset.filename.as_str())
            .collect()
    }

    /// One zip per packager flavor, each with a nolib twin listed first.
    fn bigwigs() -> ReleaseSet {
        let flavors = [
            ("mainline", 120_105),
            ("classic", 11_509),
            ("bcc", 20_506),
            ("wrath", 38_002),
            ("titan", 38_002),
            ("cata", 40_402),
            ("mists", 50_504),
            ("forever", 16_001),
        ];
        let mut assets = Vec::new();
        for (flavor, interface) in flavors {
            assets.push(asset(
                &format!("X-{flavor}-nolib.zip"),
                &[(flavor, Some(interface))],
                &[],
                true,
            ));
            assets.push(asset(
                &format!("X-{flavor}.zip"),
                &[(flavor, Some(interface))],
                &[],
                false,
            ));
        }
        set(vec![release("1.0", Channel::Stable, assets)])
    }

    /// (client version, install kind, family label, expected first BigWigs zip)
    const FAMILIES: &[(&str, InstallKind, &str, &str)] = &[
        (
            "12.1.5.70391",
            InstallKind::Battlenet,
            "Mainline",
            "X-mainline.zip",
        ),
        (
            "1.15.9.64520",
            InstallKind::Battlenet,
            "Vanilla",
            "X-classic.zip",
        ),
        ("2.5.6.64702", InstallKind::Battlenet, "Tbc", "X-bcc.zip"),
        (
            "3.4.5.60000",
            InstallKind::Battlenet,
            "Wrath",
            "X-wrath.zip",
        ),
        (
            "3.80.2.63211",
            InstallKind::Battlenet,
            "Titan",
            "X-titan.zip",
        ),
        ("4.4.2.60000", InstallKind::Battlenet, "Cata", "X-cata.zip"),
        (
            "5.5.4.63854",
            InstallKind::Battlenet,
            "Mists",
            "X-mists.zip",
        ),
        (
            "1.60.1.70374",
            InstallKind::Battlenet,
            "Forever",
            "X-forever.zip",
        ),
    ];

    #[test]
    fn every_modern_family_takes_its_flavor_first() {
        let set = bigwigs();
        for (version, kind, family, first) in FAMILIES {
            let target = target(version, *kind);
            assert_eq!(format!("{:?}", target.family), *family, "{version}");
            let selection = select(&set, &target, Channel::Stable, None);
            let order = order(&selection);
            assert_eq!(order.len(), 16, "nothing dropped");
            assert_eq!(order[0], *first, "{family}: {order:?}");
            assert_eq!(
                order[1],
                first.replace(".zip", "-nolib.zip"),
                "{family}: nolib second"
            );
            assert!(
                matches!(
                    selection.candidates[0].rank,
                    RankReason::MetadataInterface | RankReason::MetadataFlavor
                ),
                "{family}"
            );
            assert_eq!(
                selection.candidates[2].rank,
                RankReason::MetadataMismatch,
                "{family}: the rest are kept, last"
            );
        }
    }

    #[test]
    fn legacy_and_unknown_targets_have_no_metadata_match() {
        let set = bigwigs();
        for version in [
            "1.12.1.5875",
            "2.4.3.8606",
            "3.3.5.12340",
            "4.3.4.15595",
            "5.4.8.18414",
            "0.0.0.0",
        ] {
            let target = target(version, InstallKind::Custom);
            assert!(
                target.family.release_flavor().is_none(),
                "{version}: {:?}",
                target.family
            );
            let selection = select(&set, &target, Channel::Stable, None);
            assert!(
                selection
                    .candidates
                    .iter()
                    .all(|c| c.rank == RankReason::MetadataMismatch),
                "{version}"
            );
            assert_eq!(
                order(&selection)[0],
                "X-mainline.zip",
                "{version}: set order, nolib last"
            );
        }
    }

    #[test]
    fn weakauras_without_mainline_metadata_is_still_a_retail_candidate() {
        let wa = set(vec![release(
            "5.22.1",
            Channel::Stable,
            vec![asset(
                "WeakAuras-5.22.1.zip",
                &[
                    ("mists", Some(50_504)),
                    ("classic", Some(11_509)),
                    ("bcc", Some(20_506)),
                ],
                &[],
                false,
            )],
        )]);
        let selection = select(
            &wa,
            &target("12.1.5.70391", InstallKind::Battlenet),
            Channel::Stable,
            None,
        );
        assert_eq!(order(&selection), ["WeakAuras-5.22.1.zip"]);
        assert_eq!(selection.candidates[0].rank, RankReason::MetadataMismatch);
    }

    #[test]
    fn interfaces_break_ties_inside_the_metadata_tier() {
        let multi = set(vec![release(
            "1.0",
            Channel::Stable,
            vec![
                asset("old.zip", &[("mainline", Some(110_200))], &[], false),
                asset("near.zip", &[("mainline", Some(120_100))], &[], false),
                asset("exact.zip", &[("mainline", Some(120_105))], &[], false),
                asset(
                    "multi.zip",
                    &[("classic", Some(11_509)), ("mainline", None)],
                    &[],
                    false,
                ),
            ],
        )]);
        let selection = select(
            &multi,
            &target("12.1.5.70391", InstallKind::Battlenet),
            Channel::Stable,
            None,
        );
        assert_eq!(
            order(&selection),
            ["exact.zip", "near.zip", "old.zip", "multi.zip"]
        );
        assert_eq!(selection.candidates[0].rank, RankReason::MetadataInterface);
        assert_eq!(selection.candidates[1].rank, RankReason::MetadataFlavor);
    }

    #[test]
    fn name_hints_rank_between_metadata_and_mismatches() {
        let rest = set(vec![release(
            "2.0",
            Channel::Stable,
            vec![
                asset("Foo-classic.zip", &[], &[FlavorHint::ClassicAny], false),
                asset("Foo-mainline.zip", &[], &[FlavorHint::Mainline], false),
                asset("Foo-3.3.5.zip", &[], &[], false),
            ],
        )]);
        let retail = select(
            &rest,
            &target("12.1.5.70391", InstallKind::Battlenet),
            Channel::Stable,
            None,
        );
        assert_eq!(
            order(&retail),
            ["Foo-mainline.zip", "Foo-3.3.5.zip", "Foo-classic.zip"]
        );
        assert_eq!(
            retail.candidates.iter().map(|c| c.rank).collect::<Vec<_>>(),
            [
                RankReason::NameHint,
                RankReason::Unannotated,
                RankReason::NameHintMismatch
            ]
        );
        let era = select(
            &rest,
            &target("1.15.9.64520", InstallKind::Battlenet),
            Channel::Stable,
            None,
        );
        assert_eq!(order(&era)[0], "Foo-classic.zip");
        let wotlk = select(
            &rest,
            &target("3.3.5.12340", InstallKind::Custom),
            Channel::Stable,
            None,
        );
        assert_eq!(
            order(&wotlk),
            ["Foo-3.3.5.zip", "Foo-classic.zip", "Foo-mainline.zip"],
            "hints never match legacy"
        );
    }

    #[test]
    fn unannotated_single_assets_suit_every_family() {
        let git = set(vec![release(
            "abc1234",
            Channel::Stable,
            vec![asset("checkout", &[], &[], false)],
        )]);
        for version in ["12.1.5.70391", "1.15.9.64520", "3.3.5.12340", "0.0.0.0"] {
            let kind = if version.starts_with("3.3") || version.starts_with("0.") {
                InstallKind::Custom
            } else {
                InstallKind::Battlenet
            };
            let selection = select(&git, &target(version, kind), Channel::Stable, None);
            assert_eq!(
                (order(&selection), selection.candidates[0].rank),
                (vec!["checkout"], RankReason::Unannotated),
                "{version}"
            );
        }
    }

    fn channels() -> ReleaseSet {
        let one = |v: &str, ch| release(v, ch, vec![asset(&format!("{v}.zip"), &[], &[], false)]);
        set(vec![
            one("1.2-alpha", Channel::Alpha),
            one("1.1-beta", Channel::Beta),
            one("1.0", Channel::Stable),
        ])
    }

    #[test]
    fn channels_and_pins() {
        let retail = target("12.1.5.70391", InstallKind::Battlenet);
        let set = channels();
        let first = |ch, pin| {
            select(&set, &retail, ch, pin)
                .candidates
                .first()
                .map(|c| c.version.clone())
        };
        assert_eq!(first(Channel::Stable, None).as_deref(), Some("1.0"));
        assert_eq!(first(Channel::Beta, None).as_deref(), Some("1.1-beta"));
        assert_eq!(first(Channel::Alpha, None).as_deref(), Some("1.2-alpha"));
        assert_eq!(first(Channel::Stable, Some("1.0")).as_deref(), Some("1.0"));
        assert_eq!(first(Channel::Stable, Some("v1.0")).as_deref(), Some("1.0"));
        assert_eq!(
            first(Channel::Stable, Some("1.2-alpha")).as_deref(),
            Some("1.2-alpha"),
            "a pin ignores the channel"
        );
        assert_eq!(
            select(&set, &retail, Channel::Stable, Some("9.9")).empty_reason,
            Some(EmptyReason::PinnedVersionNotFound)
        );
        let betas_only = ReleaseSet {
            releases: set.releases[..2].to_vec(),
            ..set.clone()
        };
        assert_eq!(
            select(&betas_only, &retail, Channel::Stable, None).empty_reason,
            Some(EmptyReason::NoEligibleRelease)
        );
        let bare = ReleaseSet {
            releases: vec![release("1.0", Channel::Stable, vec![])],
            ..set.clone()
        };
        assert_eq!(
            select(&bare, &retail, Channel::Stable, None).empty_reason,
            Some(EmptyReason::NoAssets)
        );
        let vtag = ReleaseSet {
            releases: vec![Release {
                release_id: "gh:o/r@v2.0".into(),
                ..release(
                    "2.0.0",
                    Channel::Stable,
                    vec![asset("a.zip", &[], &[], false)],
                )
            }],
            ..set
        };
        assert_eq!(
            select(&vtag, &retail, Channel::Stable, Some("2.0"))
                .candidates
                .len(),
            1,
            "matches the tag without its v"
        );
    }

    #[test]
    fn published_at_decides_when_every_release_has_it() {
        let retail = target("12.1.5.70391", InstallKind::Battlenet);
        let dated = |v: &str, at: &str| Release {
            published_at: Some(at.parse().unwrap()),
            ..release(v, Channel::Stable, vec![asset("a.zip", &[], &[], false)])
        };
        let s = set(vec![
            dated("1.0", "2026-01-01T00:00:00Z"),
            dated("1.1", "2026-03-01T00:00:00Z"),
        ]);
        assert_eq!(
            select(&s, &retail, Channel::Stable, None).candidates[0].version,
            "1.1"
        );
    }

    #[test]
    fn selection_does_no_io() {
        let source = include_str!("select.rs");
        let code = source.split("#[cfg(test)]").next().unwrap_or_default();
        for banned in [
            "std::fs",
            "std::net",
            "crate::net",
            "std::env",
            "SystemTime",
            "Timestamp::now",
        ] {
            assert!(!code.contains(banned), "select.rs uses {banned}");
        }
    }

    proptest! {
        #[test]
        fn candidates_are_a_permutation_of_the_chosen_release(
            picks in proptest::collection::vec((0usize..10, any::<bool>(), 0usize..4), 0..12),
        ) {
            let flavors = ["mainline", "classic", "bcc", "wrath", "titan", "cata", "mists", "forever", "tbc", "future"];
            let assets: Vec<Asset> = picks
                .iter()
                .enumerate()
                .map(|(i, (flavor, nolib, kind))| match kind {
                    0 => asset(&format!("{i}.zip"), &[(flavors[*flavor], Some(120_105))], &[], *nolib),
                    1 => asset(&format!("{i}.zip"), &[], &[FlavorHint::ClassicAny], *nolib),
                    _ => asset(&format!("{i}.zip"), &[], &[], *nolib),
                })
                .collect();
            let s = set(vec![release("1.0", Channel::Stable, assets.clone())]);
            for (version, kind, _, _) in FAMILIES {
                let selection = select(&s, &target(version, *kind), Channel::Stable, None);
                let mut got: Vec<String> = selection.candidates.iter().map(|c| c.asset.filename.clone()).collect();
                let mut want: Vec<String> = assets.iter().map(|a| a.filename.clone()).collect();
                got.sort();
                want.sort();
                prop_assert_eq!(got, want);
            }
        }
    }
}
