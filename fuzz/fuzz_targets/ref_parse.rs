//! What users type to name an addon: any text is a canonical ref or a typed error. The
//! canonical form re-parses to itself.
#![no_main]

use std::path::PathBuf;

use libfuzzer_sys::fuzz_target;
use omaddons::model::Ref;
use omaddons::sources::refs::{RefContext, parse_user_ref};

fuzz_target!(|data: &[u8]| {
    let input = String::from_utf8_lossy(data);
    if let Ok(canonical) = input.parse::<Ref>() {
        assert_eq!(canonical.to_string().parse::<Ref>().ok(), Some(canonical));
    }
    let hosts = vec!["codeberg.org".to_owned()];
    let ctx = RefContext {
        cwd: PathBuf::from("/nonexistent-omaddons-fuzz"),
        git_hosts: &hosts,
        installed: &|_| Vec::new(),
        catalog: &|_| None,
    };
    let _ = parse_user_ref(&input, &ctx);
});
