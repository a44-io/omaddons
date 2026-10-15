//! Steam `libraryfolders.vdf`: any text gives its library paths, or none.
#![no_main]

use libfuzzer_sys::fuzz_target;
use omaddons::discover::launchers::steam::library_paths;

fuzz_target!(|data: &[u8]| {
    let _ = library_paths(&String::from_utf8_lossy(data));
});
