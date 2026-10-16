//! Release-note BBCode as WoWInterface might send it: any text becomes bounded plain
//! text with no markup and no control characters but newline and tab. The first two
//! bytes are `max_chars`.
#![no_main]

use libfuzzer_sys::fuzz_target;
use omaddons::text::{bbcode_to_text, has_markup};

fuzz_target!(|data: &[u8]| {
    let (max, rest) = match data {
        [a, b, rest @ ..] => (usize::from(u16::from_le_bytes([*a, *b])), rest),
        _ => (0, data),
    };
    let input = String::from_utf8_lossy(rest);
    let out = bbcode_to_text(&input, max);
    assert!(out.chars().count() <= max + 1);
    assert!(out.chars().all(|c| !c.is_control() || c == '\n' || c == '\t'));
    assert!(!has_markup(&out), "markup in {out:?}");
});
