//! WoWInterface's file list as the API might send it: any bytes decode to slim entries
//! or a typed error, and every kept folder name is valid, not junk, and unique ignoring
//! case.
#![no_main]

use libfuzzer_sys::fuzz_target;
use omaddons::sources::wowi::index::decode_filelist;

fuzz_target!(|data: &[u8]| {
    if let Ok((entries, _)) = decode_filelist(data) {
        for entry in entries {
            let keys: Vec<&str> = entry.dirs.iter().map(|d| d.key()).collect();
            for (i, key) in keys.iter().enumerate() {
                assert!(!key.is_empty() && !key.starts_with('.'));
                assert!(!["__macosx", "interface", "wtf"].contains(key));
                assert!(!keys[..i].contains(key), "a case duplicate survived");
            }
        }
    }
});
