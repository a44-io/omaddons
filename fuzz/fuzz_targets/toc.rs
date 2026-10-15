//! TOC files: any bytes parse (lossily) into a `Toc`.
#![no_main]

use libfuzzer_sys::fuzz_target;
use omaddons::toc::parse_toc;

fuzz_target!(|data: &[u8]| {
    let _ = parse_toc(data);
});
