//! WoWInterface `filedetails` as the API might send it: any bytes decode to entries or a
//! typed error, and each entry makes one release identified by its UID, with at most
//! one asset, an MD5 only when it is 32 lowercase hex digits, and notes that are
//! bounded plain text.
#![no_main]

use libfuzzer_sys::fuzz_target;
use omaddons::limits::CHANGELOG_MAX_CHARS;
use omaddons::sources::wowi::{decode_details, release};
use omaddons::text::has_markup;

fuzz_target!(|data: &[u8]| {
    if let Ok(entries) = decode_details(data) {
        for details in entries {
            let release = release(&details);
            assert!(
                release
                    .release_id
                    .starts_with(&format!("wowi:{}@", details.uid))
            );
            assert!(!release.version.is_empty());
            assert!(release.assets.len() <= 1);
            for asset in &release.assets {
                if let Some(hash) = &asset.expected_hash {
                    let hex = hash.hex();
                    assert!(hex.len() == 32);
                    assert!(hex.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));
                }
            }
            if let Some(notes) = &release.notes {
                assert!(!notes.is_empty() && notes.chars().count() <= CHANGELOG_MAX_CHARS + 1);
                assert!(!has_markup(notes), "markup in {notes:?}");
            }
        }
    }
});
