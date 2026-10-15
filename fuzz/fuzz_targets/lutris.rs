//! Lutris `games/*.yml`: any text names an exe, a prefix and a working dir, or nothing.
#![no_main]

use std::path::Path;

use libfuzzer_sys::fuzz_target;
use omaddons::discover::launchers::lutris::parse;

fuzz_target!(|data: &[u8]| {
    let game = parse(
        &String::from_utf8_lossy(data),
        Path::new("/nonexistent-omaddons-fuzz"),
    );
    let _ = game.client_exe();
});
