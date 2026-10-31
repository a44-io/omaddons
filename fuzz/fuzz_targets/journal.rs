//! Journals of both families (E11, E5's repairs): any bytes parse and diagnose without a
//! panic, the same bytes always diagnose the same, every diagnosis agrees with the parse
//! and names nothing below `WTF/`, and a diagnosis on disk never writes.
#![no_main]

use std::path::PathBuf;
use std::sync::OnceLock;

use libfuzzer_sys::fuzz_target;
use omaddons::engine::simulate::{fuzz_disk_property, fuzz_property};
use omaddons::model::TxId;
use omaddons::state::paths::Paths;

/// One scratch home per process, reused for every input: its two journals, a folder
/// journal and a repair journal, are the input.
fn scratch() -> &'static (Paths, [PathBuf; 2]) {
    static HOME: OnceLock<(Paths, [PathBuf; 2])> = OnceLock::new();
    HOME.get_or_init(|| {
        let root = std::env::temp_dir().join(format!("omaddons-fuzz-journal-{}", std::process::id()));
        let paths = Paths::with_roots(&root, root.join("c"), root.join("s"), root.join("k"));
        std::fs::create_dir_all(paths.journal_dir()).unwrap();
        let journals = [paths.journal(&TxId::new()), paths.journal_repair(&TxId::new())];
        (paths, journals)
    })
}

fuzz_target!(|data: &[u8]| {
    fuzz_property(data);
    // The disk arm on one input in 16, by its last byte: it costs two writes and two walks
    // of the scratch home, which held the in-memory arms under 2,000 runs a second.
    if data.last().is_some_and(|byte| byte % 16 == 0) {
        let (paths, journals) = scratch();
        // The harness's own writes, before the measurement.
        for journal in journals {
            std::fs::write(journal, data).unwrap();
        }
        fuzz_disk_property(paths);
    }
});
