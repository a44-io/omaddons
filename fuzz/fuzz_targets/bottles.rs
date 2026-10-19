//! Bottles `bottle.yml`, `placeholder.yml` and `data.yml`: any text gives a bottle path
//! or a moved bottles directory, or none.
#![no_main]

use std::path::Path;

use libfuzzer_sys::fuzz_target;
use omaddons::discover::launchers::bottles::top_level_path;

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    for key in ["Path", "custom_bottles_path"] {
        let _ = top_level_path(&text, key, Path::new("/nonexistent-omaddons-fuzz"));
    }
});
