//! BPSV (`.build.info`, `.flavor.info`): any text parses or fails with a typed error.
#![no_main]

use libfuzzer_sys::fuzz_target;
use omaddons::discover::bpsv::{Bpsv, build_info_rows, flavor_product};

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    if let Ok(table) = Bpsv::parse(&text) {
        for row in table.rows() {
            let _ = row.get("Product");
        }
    }
    let _ = build_info_rows(&text);
    let _ = flavor_product(&text);
});
