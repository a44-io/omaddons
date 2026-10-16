//! TOC files: any bytes parse (lossily) into a `Toc`; its dependencies evaluate on every
//! builtin rules row into valid folder names, a name never both required and under an
//! unknown condition; and a plain title never panics or grows.
#![no_main]

use std::sync::OnceLock;

use libfuzzer_sys::fuzz_target;
use omaddons::rules::{ClientRules, RulesTable};
use omaddons::toc::{parse_toc, plain_title};

fuzz_target!(|data: &[u8]| {
    static ROWS: OnceLock<Vec<ClientRules>> = OnceLock::new();
    let rows = ROWS.get_or_init(|| RulesTable::builtin().client_rules().to_vec());
    let toc = parse_toc(data);
    for rules in rows {
        let deps = toc.dependencies_for(rules);
        assert!(
            deps.unknown_condition
                .iter()
                .all(|d| !deps.required.contains(d))
        );
        let _ = toc.title_for(rules);
    }
    if let Some(title) = &toc.title {
        assert!(plain_title(title).chars().count() <= title.chars().count());
    }
});
