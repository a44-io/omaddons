//! Bottles `bottle.yml`: any text gives a custom bottle path, or none.
#![no_main]

use std::path::Path;

use libfuzzer_sys::fuzz_target;
use omaddons::discover::launchers::bottles::custom_path;

fuzz_target!(|data: &[u8]| {
    let _ = custom_path(
        &String::from_utf8_lossy(data),
        Path::new("/nonexistent-omaddons-fuzz"),
    );
});
