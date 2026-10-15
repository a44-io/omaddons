//! Every exit maps to a published code (vision §5.9 publication rules): each
//! `ErrorCode::<Variant>` the source uses is registered, every registered code has a
//! unique snake_case wire name and an exit in the general dictionary (0–6), and the
//! process leaves only through `Exit` (no `std::process::exit`; `ExitCode::from` only
//! where an `Exit` is converted).

use super::*;
use omaddons::error::Exit;
use std::collections::BTreeSet;
use support::sources;

#[test]
fn every_error_code_used_in_src_is_registered() {
    let registered: BTreeSet<String> = ErrorCode::ALL.iter().map(|c| format!("{c:?}")).collect();
    let mut used = BTreeSet::new();
    for (_, text) in sources() {
        for (at, _) in text.match_indices("ErrorCode::") {
            let rest = &text[at + "ErrorCode::".len()..];
            let name: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric())
                .collect();
            if name.chars().next().is_some_and(|c| c.is_ascii_uppercase()) && name != "ALL" {
                used.insert(name);
            }
        }
    }
    let unregistered: Vec<&String> = used.difference(&registered).collect();
    assert!(
        unregistered.is_empty(),
        "used but not registered: {unregistered:?}"
    );
    assert!(used.len() > 20, "the scan found the codes: {used:?}");
}

#[test]
fn registered_codes_are_unique_snake_case_with_dictionary_exits() {
    let dictionary: BTreeSet<u8> = Exit::ALL.iter().map(|e| e.code()).collect();
    assert_eq!(dictionary, (0..=6).collect::<BTreeSet<u8>>());
    let mut wires = BTreeSet::new();
    for code in ErrorCode::ALL {
        let wire = code.wire();
        assert!(
            wire.chars().all(|c| c.is_ascii_lowercase() || c == '_') && !wire.starts_with('_'),
            "{wire} is not snake_case"
        );
        assert!(wires.insert(wire), "{wire} is registered twice");
        assert!(
            dictionary.contains(&code.exit().code()),
            "{wire} exits outside 0–6"
        );
        assert_ne!(code.exit().code(), 0, "{wire} is an error that exits 0");
    }
}

#[test]
fn the_process_exits_only_through_exit() {
    for (path, text) in sources() {
        let code = text.split("#[cfg(test)]").next().unwrap_or_default();
        let name = path.display().to_string();
        assert!(
            !code.contains("std::process::exit"),
            "{name} calls std::process::exit"
        );
        if code.contains("ExitCode::from") {
            assert!(
                name.ends_with("src/cli.rs")
                    || name.ends_with("src/error.rs")
                    || name.ends_with("src/main.rs"),
                "{name} builds an ExitCode outside the Exit conversion"
            );
        }
    }
}
