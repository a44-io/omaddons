//! Journals (E11): any bytes parse and diagnose without a panic, the same bytes always
//! diagnose the same, every diagnosis agrees with the parse and names nothing below
//! `WTF/`, and a diagnosis on disk never writes. E5 extends it to repair journals.
#![no_main]

use std::path::PathBuf;
use std::sync::OnceLock;

use libfuzzer_sys::fuzz_target;
use omaddons::engine::simulate::{fuzz_disk_property, fuzz_property};
use omaddons::model::TxId;
use omaddons::state::paths::Paths;

/// One scratch home per process, reused for every input: its journal is the input.
fn scratch() -> &'static (Paths, PathBuf) {
    static HOME: OnceLock<(Paths, PathBuf)> = OnceLock::new();
    HOME.get_or_init(|| {
        let root = std::env::temp_dir().join(format!("omaddons-fuzz-journal-{}", std::process::id()));
        let paths = Paths::with_roots(&root, root.join("c"), root.join("s"), root.join("k"));
        std::fs::create_dir_all(paths.journal_dir()).unwrap();
        let journal = paths.journal(&TxId::new());
        (paths, journal)
    })
}

fuzz_target!(|data: &[u8]| {
    fuzz_property(data);
    // The disk arm on one input in 16, by its last byte: it costs a write and two walks
    // of the scratch home, which held the in-memory arms under 2,000 runs a second.
    if data.last().is_some_and(|byte| byte % 16 == 0) {
        let (paths, journal) = scratch();
        // The harness's own write, before the measurement.
        std::fs::write(journal, data).unwrap();
        fuzz_disk_property(paths);
    }
});
