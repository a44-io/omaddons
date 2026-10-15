//! Battle.net's `product.db` (protobuf wire format): any bytes give entries or none.
#![no_main]

use libfuzzer_sys::fuzz_target;
use omaddons::discover::product_db::parse_product_db;

fuzz_target!(|data: &[u8]| {
    let _ = parse_product_db(data);
});
