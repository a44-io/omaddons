//! Heroic's JSON files: any text gives a `GamesConfig` file's `winePrefix` strings,
//! `config.json`'s prefix settings and `library.json`'s sideloaded exes, or JSON errors.
#![no_main]

use libfuzzer_sys::fuzz_target;
use omaddons::discover::launchers::heroic::{settings, sideloaded, wine_prefixes};

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    let _ = wine_prefixes(&text);
    let _ = settings(&text);
    let _ = sideloaded(&text);
});
