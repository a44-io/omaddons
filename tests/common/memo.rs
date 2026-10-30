//! The release memo, seeded for tests: entries as a fetch at a chosen time would have
//! left them, so freshness tests need no clock seam.

use jiff::Timestamp;
use omaddons::model::{Asset, AssetFlavor, AssetLocation, Channel, Ref, Release, ReleaseSet};
use omaddons::state::memo::{self, MemoEntry, ReleaseMemo};

use super::home::Home;
use super::http::paths;

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
            revision: None,
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

/// The home's release memo aged by `hours`: every entry's `fetched_at`, the Tukui list's
/// and every look-back's `at` moved back. The commands' 15-minute window has passed: the
/// next window command asks its sources again.
pub fn age(home: &Home, hours: i64) {
    let by = jiff::SignedDuration::from_hours(hours);
    let (mut memo, _) = memo::load(&paths(&home.env));
    for entry in memo.entries.values_mut() {
        entry.fetched_at -= by;
    }
    if let Some(list) = memo.tukui_list.as_mut() {
        list.fetched_at -= by;
    }
    for look_back in memo.look_backs.values_mut() {
        look_back.at -= by;
    }
    home.seed_memo(&memo);
}

/// `minutes` before now.
pub fn ago(minutes: i64) -> Timestamp {
    Timestamp::now() - jiff::SignedDuration::from_mins(minutes)
}
