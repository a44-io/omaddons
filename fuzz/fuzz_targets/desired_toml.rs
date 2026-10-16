//! `addons.toml` as people write it: any text is desired state or a typed error, and
//! what parses renders to a file that parses back to the same rendering.
#![no_main]

use std::path::Path;

use libfuzzer_sys::fuzz_target;
use omaddons::state::desired::{parse, render};

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    let path = Path::new("addons.toml");
    if let Ok((state, _)) = parse(&text, path) {
        let rendered = render(&state);
        let again = parse(&rendered, path).map(|(s, _)| render(&s));
        assert_eq!(again.ok().as_deref(), Some(rendered.as_str()));
    }
});
