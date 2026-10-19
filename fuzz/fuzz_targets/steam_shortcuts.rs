//! Steam `shortcuts.vdf`: any bytes give their shortcuts, or none, and each one's
//! client exe.
#![no_main]

use std::path::Path;

use libfuzzer_sys::fuzz_target;
use omaddons::discover::launchers::steam::shortcuts;

fuzz_target!(|data: &[u8]| {
    for shortcut in shortcuts(data) {
        let _ = shortcut.client_exe(Path::new("/h"));
    }
});
