//! Every exit maps to a published code (vision §5.9 publication rules): each
//! `ErrorCode::<Variant>` the source uses is registered, every registered code has a
//! unique snake_case wire name and an exit in the general dictionary (0–6) and one in the
//! doctor's (M4-D40), the doctor tree exits only through its own dictionary, and the
//! process leaves only through a `ProcessExit` (no `std::process::exit`; `ExitCode::from`
//! only where one is converted).

use super::*;
use omaddons::doctor::exit::{Exit as DoctorExit, exit_for};
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
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            // Associated constants (`ALL`, `CONDITIONALLY_RETRYABLE`) are no variants.
            let constant = name
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
            if name.chars().next().is_some_and(|c| c.is_ascii_uppercase()) && !constant {
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
                "{name} builds an ExitCode outside the ProcessExit conversion"
            );
        }
    }
}

#[test]
fn doctor_exit_dictionary_is_the_visions() {
    let expected = [
        (0, "healthy"),
        (1, "findings_present"),
        (2, "fix_partial"),
        (3, "fix_failed_rolled_back"),
        (4, "refused_unsafe"),
        (5, "concurrency_lost"),
        (6, "online_required"),
        (64, "usage_error"),
        (66, "no_input"),
        (73, "cannot_create"),
        (74, "io_error"),
    ];
    let actual: Vec<(u8, &str)> = DoctorExit::ALL
        .iter()
        .map(|exit| (exit.code(), exit.name()))
        .collect();
    assert_eq!(
        actual, expected,
        "vision D18's codes and names, in code order"
    );
    let names: BTreeSet<&str> = actual.iter().map(|(_, name)| *name).collect();
    assert_eq!(names.len(), expected.len(), "unique names");
    for (_, name) in actual {
        assert!(
            name.chars().all(|c| c.is_ascii_lowercase() || c == '_') && !name.starts_with('_'),
            "{name} is not snake_case"
        );
    }
}

/// The class rule, restated by general exit status: a code's own row first.
#[test]
fn every_error_code_has_a_doctor_exit() {
    for code in ErrorCode::ALL {
        let expected = match (code.wire(), code.exit().code()) {
            ("plan_stale", _) => 4,
            ("check_failed", _) => 1,
            ("online_required", _) => 6,
            (_, 2) => 64,
            (_, 3) => 66,
            (_, 4) => 4,
            (_, 5) => 5,
            (_, 1) => 74,
            (wire, general) => panic!("{wire}: general exit {general} has no doctor class"), // ubs:ignore — a code without a class fails the test, naming it
        };
        let exit = exit_for(*code);
        assert_eq!(
            exit.code(),
            expected,
            "{}: general exit {}, doctor exit {}",
            code.wire(),
            code.exit().code(),
            exit.name()
        );
        assert_ne!(exit, DoctorExit::Healthy, "{} exits healthy", code.wire());
    }
}

/// The general dictionary's variants that `text` names as `Exit::<Variant>`, its test
/// items and line comments aside. Doctor variants that merely start alike
/// (`Exit::UsageError`, `Exit::RefusedUnsafe`) are not them.
fn general_exits_named(text: &str) -> Vec<&'static str> {
    let production = text.split("#[cfg(test)]").next().unwrap_or_default();
    let code: String = production
        .lines()
        .map(|line| line.split("//").next().unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n");
    let mut found = Vec::new();
    for exit in Exit::ALL {
        let variant = format!("{exit:?}");
        let needle = format!("Exit::{variant}");
        let named = code.match_indices(&needle).any(|(at, _)| {
            code[at + needle.len()..]
                .chars()
                .next()
                .is_none_or(|c| !(c.is_ascii_alphanumeric() || c == '_'))
        });
        if named {
            found.push(exit.name());
        }
    }
    found
}

/// The doctor tree (its command module and `src/doctor/`) exits only through the doctor's
/// dictionary: no general `Exit::` variant outside `src/doctor/exit.rs`, the mapping
/// itself. A file that does not exist yet passes.
#[test]
fn the_doctor_tree_exits_only_through_its_dictionary() {
    let mut scanned = 0;
    let mut found = Vec::new();
    for (path, text) in sources() {
        let name = path.display().to_string();
        let in_tree = name.ends_with("src/app/doctor.rs")
            || (name.contains("/src/doctor/") && !name.ends_with("src/doctor/exit.rs"));
        if !in_tree {
            continue;
        }
        scanned += 1;
        for exit in general_exits_named(&text) {
            found.push(format!("{name}: Exit::{exit}"));
        }
    }
    assert!(
        found.is_empty(),
        "general exits in the doctor tree (use doctor::exit::Exit):\n{}",
        found.join("\n")
    );
    assert!(scanned > 0, "the scan found the doctor tree");

    // A planted one is seen; the doctor's own variants, comments and tests are not.
    assert_eq!(
        general_exits_named("fn f() -> Exit { Exit::Failed }"),
        ["failed"]
    );
    assert_eq!(
        general_exits_named("error::Exit::Refused => x, error::Exit::Usage,"),
        ["usage", "refused"]
    );
    assert!(
        general_exits_named(
            "DoctorExit::UsageError; Exit::RefusedUnsafe; Exit::IoError\n\
             // Exit::Failed in a comment\n\
             #[cfg(test)]\nmod tests { const X: Exit = Exit::Ok; }"
        )
        .is_empty()
    );
}

/// Reason enums may occur inside refused-folder arrays as well as at the root.
fn declared_reasons(spec: &omaddons::error::DetailSpec, out: &mut BTreeSet<&'static str>) {
    use omaddons::error::DetailSpec;
    match spec {
        DetailSpec::Object(fields) => {
            for field in *fields {
                if field.key == "reason"
                    && let DetailSpec::Enum(values) = field.spec
                {
                    out.extend(values.iter().copied());
                }
                declared_reasons(field.spec, out);
            }
        }
        DetailSpec::Array(element) => declared_reasons(element, out),
        DetailSpec::OneOf(arms) => {
            for arm in *arms {
                declared_reasons(arm, out);
            }
        }
        _ => {}
    }
}

/// One ordinary quoted literal, including escaped quotes in its boundary scan.
fn reason_literal(text: &str) -> Option<String> {
    let text = text.trim_start();
    if !text.starts_with('"') {
        return None;
    }
    let mut escaped = false;
    for (at, c) in text.char_indices().skip(1) {
        match (escaped, c) {
            (true, _) => escaped = false,
            (false, '\\') => escaped = true,
            (false, '"') => return serde_json::from_str(&text[..=at]).ok(),
            _ => {}
        }
    }
    None
}

/// The nth argument, balanced like `text_fix_literals`, with commas inside nested
/// calls, arrays, objects and quoted strings kept in their argument.
fn reason_argument(code: &str, nth: usize) -> Option<String> {
    let (mut depth, mut in_str, mut escaped) = (0usize, false, false);
    let (mut start, mut argument) = (0, 0);
    for (at, c) in code.char_indices() {
        if in_str {
            match (escaped, c) {
                (true, _) => escaped = false,
                (false, '\\') => escaped = true,
                (false, '"') => in_str = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            ')' if depth == 0 => {
                return (argument == nth)
                    .then(|| reason_literal(&code[start..at]))
                    .flatten();
            }
            ',' if depth == 0 => {
                if argument == nth {
                    return reason_literal(&code[start..at]);
                }
                argument += 1;
                start = at + 1;
            }
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth = depth.checked_sub(1)?,
            _ => {}
        }
    }
    None
}

#[test]
fn every_details_reason_literal_is_declared() {
    // No production data shape currently has a reason literal unrelated to errors.
    const NOT_ERROR_DETAILS: &[(&str, &str)] = &[];
    let mut declared = BTreeSet::new();
    for code in ErrorCode::ALL {
        declared_reasons(code.detail_spec(), &mut declared);
    }
    let mut used = Vec::new();
    for (path, text) in sources() {
        if matches!(
            path.file_name().and_then(|n| n.to_str()),
            Some("tests.rs" | "testing.rs")
        ) {
            continue;
        }
        let code = text
            .split("\n#[cfg(test)]\nmod tests {")
            .next()
            .unwrap_or(&text);
        let mut found = Vec::new();
        for (at, token) in code.match_indices("\"reason\"") {
            let rest = code[at + token.len()..].trim_start();
            if let Some(value) = rest.strip_prefix(':').and_then(reason_literal) {
                found.push((at, value));
            }
        }
        let helpers: &[(&str, usize)] = if path.ends_with("engine/inspect.rs") {
            &[("malformed(", 1)]
        } else if path.ends_with("backup/import.rs") {
            &[("refused(", 0), ("bad(", 0)]
        } else if path.ends_with("engine/ownership.rs") {
            &[("refuse(", 2)]
        } else {
            &[]
        };
        for &(call, argument) in helpers {
            for (at, _) in code.match_indices(call) {
                if let Some(value) = reason_argument(&code[at + call.len()..], argument) {
                    found.push((at, value));
                }
            }
        }
        for (at, value) in found {
            let file = path
                .strip_prefix(env!("CARGO_MANIFEST_DIR"))
                .unwrap_or(&path)
                .display()
                .to_string();
            let line = code[..at].bytes().filter(|b| *b == b'\n').count() + 1;
            if !NOT_ERROR_DETAILS.contains(&(file.as_str(), value.as_str())) {
                used.push((file, line, value));
            }
        }
    }
    let missing: Vec<_> = used
        .iter()
        .filter(|(_, _, value)| !declared.contains(value.as_str()))
        .map(|(file, line, value)| format!("{file}:{line}: undeclared reason {value:?}"))
        .collect();
    assert!(missing.is_empty(), "{}", missing.join("\n"));
    assert!(
        used.len() >= 30,
        "reason scan found only {} literals: {used:?}",
        used.len()
    );
    assert_eq!(
        reason_argument(r#"format!("nested, {}", call([1, 2])), "corrupt")"#, 1),
        Some("corrupt".into())
    );
    assert_eq!(
        reason_argument(r#"&a, &b, "unmanaged", json!({"x": [1, 2]}))"#, 2),
        Some("unmanaged".into())
    );
    assert_eq!(
        reason_argument(r#""bad_name", other())"#, 0),
        Some("bad_name".into())
    );
    assert_eq!(
        reason_literal(r#""escaped\"quote""#),
        Some("escaped\"quote".into())
    );
}
