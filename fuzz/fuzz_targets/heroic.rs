//! Heroic `GamesConfig/*.json`: any text gives its `winePrefix` strings or a JSON error.
#![no_main]

use libfuzzer_sys::fuzz_target;
use omaddons::discover::launchers::heroic::wine_prefixes;

fuzz_target!(|data: &[u8]| {
    let _ = wine_prefixes(&String::from_utf8_lossy(data));
});
