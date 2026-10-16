//! Tukui's API as it might answer: any bytes decode, as the project list or as one
//! project, to projects or a typed error. Every project has a lowercase URL-safe slug,
//! and makes one release whose one asset is `Deferred` on that slug, with known flavors;
//! a decoded download URL is https.
#![no_main]

use std::sync::OnceLock;

use libfuzzer_sys::fuzz_target;
use omaddons::model::AssetLocation;
use omaddons::rules::Family;
use omaddons::sources::tukui::{TukuiAddon, TukuiSource, decode_addon, decode_list};

fn check(source: &TukuiSource, addon: &TukuiAddon) {
    let slug = &addon.slug;
    assert!(!slug.is_empty());
    assert!(
        slug.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
    );
    let release = source.release(addon);
    assert_eq!(release.release_id, format!("tukui:{slug}@{}", release.version));
    assert_eq!(release.assets.len(), 1);
    let asset = &release.assets[0];
    assert!(matches!(&asset.location, AssetLocation::Deferred { key, .. } if key == slug));
    assert!(
        asset
            .flavors
            .iter()
            .all(|f| Family::from_release_flavor(&f.flavor).is_some())
    );
    assert_eq!(addon.entry().slug, *slug);
}

fuzz_target!(|data: &[u8]| {
    static SOURCE: OnceLock<TukuiSource> = OnceLock::new();
    let source = SOURCE.get_or_init(TukuiSource::default);
    if let Ok(addons) = decode_list(data) {
        for addon in &addons {
            check(source, addon);
        }
    }
    if let Ok((addon, url)) = decode_addon(data) {
        check(source, &addon);
        assert!(url.is_none_or(|u| u.starts_with("https://")));
    }
});
