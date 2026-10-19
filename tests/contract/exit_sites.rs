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

/// The string literals inside each `Suggestion::text(…)` call of `code`, joined per
/// call (a `format!`'s braces kept).
fn text_fix_literals(code: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (at, call) in code.match_indices("Suggestion::text(") {
        let (mut depth, mut in_str, mut escaped) = (1, false, false);
        let mut literals = String::new();
        for c in code[at + call.len()..].chars() {
            if in_str {
                match (escaped, c) {
                    (true, _) => {
                        literals.push(c);
                        escaped = false;
                    }
                    (false, '\\') => escaped = true,
                    (false, '"') => in_str = false,
                    (false, _) => literals.push(c),
                }
                continue;
            }
            match c {
                '"' => in_str = true,
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
        }
        out.push(literals);
    }
    out
}

/// No prose fix in production code names a runnable omaddons command: such a fix
/// carries argv (vision §5.9). The scan sees the sites the matrix never reaches.
#[test]
fn prose_fixes_name_no_runnable_command() {
    let mut found = Vec::new();
    for (path, text) in sources() {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        if matches!(name, "tests.rs" | "testing.rs") {
            continue;
        }
        let code = text
            .split("\n#[cfg(test)]\nmod tests {")
            .next()
            .unwrap_or(&text);
        for display in text_fix_literals(code) {
            if support::names_a_command(&display) {
                found.push(format!("{}: {display}", path.display()));
            }
        }
    }
    assert!(
        found.is_empty(),
        "prose fixes that are omaddons commands (give them argv):\n{}",
        found.join("\n")
    );
    let planted = text_fix_literals("Err(x(Suggestion::text(\"omaddons status\")))");
    assert_eq!(planted, ["omaddons status"]);
    assert!(
        support::names_a_command(&planted[0]),
        "the scan sees a planted one"
    );
    for prose in [
        "omaddons install <path>",
        "wait for the running omaddons command to finish",
        "echo \"$TOKEN\" | omaddons auth github set",
        "/reload in game",
    ] {
        assert!(!support::names_a_command(prose), "{prose}");
    }
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
