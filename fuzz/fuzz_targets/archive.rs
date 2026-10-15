//! Archive inspection on in-memory bytes: any input is an inspected addon or a typed
//! error, with memory bounded by the archive limits. An accepted archive never names a
//! path that escapes its folder.
#![no_main]

use std::io::Cursor;
use std::sync::OnceLock;

use libfuzzer_sys::fuzz_target;
use omaddons::engine::inspect::{ArchiveLimits, inspect_zip};
use omaddons::rules::RulesTable;

fn table() -> &'static RulesTable {
    static TABLE: OnceLock<RulesTable> = OnceLock::new();
    TABLE.get_or_init(RulesTable::builtin)
}

fuzz_target!(|data: &[u8]| {
    let Ok(inspected) = inspect_zip(Cursor::new(data), &ArchiveLimits::default(), table()) else {
        return;
    };
    for folder in &inspected.folders {
        for file in &folder.files {
            let rel = &file.rel_path;
            assert!(
                !rel.starts_with('/') && !rel.split('/').any(|s| s.is_empty() || s == ".."),
                "an accepted archive names {rel:?}"
            );
        }
    }
});
