//! The release memo, seeded for tests: entries as a fetch at a chosen time would have
//! left them, so freshness tests need no clock seam.

use jiff::Timestamp;
use omaddons::model::{Asset, AssetFlavor, AssetLocation, Channel, Ref, Release, ReleaseSet};
use omaddons::state::memo::{self, MemoEntry, ReleaseMemo};

/// The stable-channel entry of `r` whose newest release is `tag`, fetched at
/// `fetched_at`, with no asset.
pub fn entry(r: &str, tag: &str, fetched_at: Timestamp) -> (String, MemoEntry) {
    with_assets(r, tag, fetched_at, Vec::new())
}

/// [`entry`] with one mainline asset downloadable from `url`.
pub fn entry_with_asset(
    r: &str,
    tag: &str,
    fetched_at: Timestamp,
    url: &str,
) -> (String, MemoEntry) {
    let asset = Asset {
        filename: url.rsplit('/').next().unwrap().to_owned(),
        location: AssetLocation::Url(url.to_owned()),
        size: None,
        expected_hash: None,
        flavors: vec![AssetFlavor {
            flavor: "mainline".into(),
            interface: Some(120_105),
            source: omaddons::model::FlavorSource::ReleaseJson,
        }],
        name_flavors: Vec::new(),
        nolib: false,
    };
    with_assets(r, tag, fetched_at, vec![asset])
}

fn with_assets(
    r: &str,
    tag: &str,
    fetched_at: Timestamp,
    assets: Vec<Asset>,
) -> (String, MemoEntry) {
    let r: Ref = r.parse().unwrap();
    let set = ReleaseSet {
        r#ref: r.clone(),
        releases: vec![Release {
            release_id: format!("{r}@{tag}"),
            version: tag.to_owned(),
            channel: Channel::Stable,
            published_at: None,
            assets,
            changelog_url: None,
            notes: None,
            name_hint: None,
        }],
    };
    let entry = MemoEntry::of(&r, Channel::Stable, &Ok(set), fetched_at).unwrap();
    (memo::key(&r, Channel::Stable), entry)
}

/// A memo holding `entries`.
pub fn of(entries: Vec<(String, MemoEntry)>) -> ReleaseMemo {
    ReleaseMemo {
        entries: entries.into_iter().collect(),
        ..ReleaseMemo::default()
    }
}

/// `minutes` before now.
pub fn ago(minutes: i64) -> Timestamp {
    Timestamp::now() - jiff::SignedDuration::from_mins(minutes)
}
