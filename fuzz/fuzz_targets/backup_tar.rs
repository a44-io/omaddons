//! An imported backup archive as it might be (M3-D35): any bytes behind the bounded zstd
//! decoder, through pass 1 (types, names, bounds, the manifest bijection and the
//! symlink graph), to a scanned archive or a typed refusal, never a panic. What passes
//! names only plain paths under `WTF/`, one payload entry per manifest entry.
#![no_main]

use libfuzzer_sys::fuzz_target;
use omaddons::backup::import::scan_reader;

fuzz_target!(|data: &[u8]| {
    if let Ok(scanned) = scan_reader(data, data.len() as u64) {
        for entry in &scanned.entries {
            assert!(!entry.path.is_empty() && !entry.path.starts_with('/'));
            assert!(entry
                .path
                .split('/')
                .all(|c| !c.is_empty() && c != "." && c != ".."));
        }
        assert_eq!(scanned.entries.len(), scanned.manifest.entries.len());
    }
});
