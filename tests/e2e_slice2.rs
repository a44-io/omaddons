//! e2e suite: slice 2's exit demo (preview, then apply exactly)
//!
//! One `demo_*` test per item of slice 2's exit demo, each run end to end through the real
//! binary on a synthetic home, logged with `TestLog`: install then `update --dry-run` then
//! `apply`; a release landing in between; a late hand edit under the lock; a second `apply`;
//! two appliers; `plans show` after a lost output; a Tukui plan from the cache; the widget's
//! Update scenarios. The slice's demo is recorded from a run of `argc e2e slice2`.

use crate::common;

use std::collections::BTreeSet;
use std::fs;
use std::io::Write as _;
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use common::home::Home;
use common::landing::{Landing, Source};
use common::memo;
use common::netlog::net_log;
use common::pause::Pause;
use common::plans;
use common::tlog::tlog;
use jiff::{SignedDuration, Timestamp};
use serde_json::{Value, json};
use support::{
    CLEANUP, Demo, PLANNED, PRESTAGED, apply_hint, assert_nothing_changed, assert_release,
    assert_untouched, baseline, behind, contains, expected_fix, names, never_planned, outcomes,
    regular_files, repo, requests, selected, sha256, stale, until_it_ends,
};

/// The demo's three widget items: the QML scenario of G1's `tst_plans.qml` that shows each,
/// and the text that scenario must assert on, so one renamed onto other behaviour, or
/// emptied, fails here.
const WIDGET_ITEMS: [(&str, &str, &str); 3] = [
    (
        "a routine matching plan applies with no dialog",
        "test_a_routine_matching_update_applies_with_no_dialog",
        "\"apply\"",
    ),
    (
        "a plan that adds a dependency opens the dialog",
        "test_a_matching_update_that_adds_a_dependency_opens_the_dialog_naming_it",
        "Also installs",
    ),
    (
        "a moved release opens the dialog",
        "test_a_moved_release_opens_the_dialog_with_changed_since_you_looked",
        "Changed since you looked",
    ),
];

/// The widget's plan scenarios (G1).
const PLANS_QML: &str = "omarchy/tests/qml/tst_plans.qml";

/// Pure readers of the widget's sources for the structural checks on its Update
/// scenarios: brace matching that skips string literals and comments, a QML suite's test
/// functions, the fixtures a body reads (directly or through a helper), a Rust array
/// constant's strings, and needles found in order. Text scanning, not parsing: it knows
/// `//` and `/* */` comments and quoted strings, which is all these files need.
#[cfg(test)]
mod widget {
    use std::collections::BTreeMap;

    fn is_ident(byte: u8) -> bool {
        byte.is_ascii_alphanumeric() || byte == b'_'
    }

    /// Where the string literal opening at `start` ends (past its closing quote): the end
    /// of `bytes` when it never closes, or `None` when `single_line` and it does not close
    /// before a newline. A backslash escapes the next byte.
    fn string_end(bytes: &[u8], start: usize, single_line: bool) -> Option<usize> {
        let quote = bytes[start];
        let mut j = start + 1;
        while j < bytes.len() {
            match bytes[j] {
                b'\\' => j += 2,
                byte if byte == quote => return Some(j + 1),
                b'\n' if single_line => return None,
                _ => j += 1,
            }
        }
        (!single_line).then_some(bytes.len())
    }

    /// `text` with every comment and string literal (quotes included) blanked to spaces,
    /// newlines kept, byte for byte: an offset into it is an offset into `text`, and a
    /// search of it finds code only. `"…"` and `` `…` `` may span lines (Rust's do); `'…'`
    /// counts only when it closes on its line (a JS string, a Rust char literal), so a
    /// Rust lifetime stays code.
    pub fn code_only(text: &str) -> String {
        let bytes = text.as_bytes();
        let mut out = bytes.to_vec();
        let mut i = 0;
        while i < bytes.len() {
            let rest = &bytes[i..];
            let end = if rest.starts_with(b"//") {
                rest.iter()
                    .position(|&byte| byte == b'\n')
                    .map_or(bytes.len(), |n| i + n)
            } else if rest.starts_with(b"/*") {
                rest[2..]
                    .windows(2)
                    .position(|pair| pair == b"*/")
                    .map_or(bytes.len(), |n| i + 2 + n + 2)
            } else if matches!(bytes[i], b'"' | b'`') {
                string_end(bytes, i, false).unwrap_or(bytes.len())
            } else if bytes[i] == b'\'' {
                match string_end(bytes, i, true) {
                    Some(end) => end,
                    None => {
                        i += 1;
                        continue;
                    }
                }
            } else {
                i += 1;
                continue;
            };
            for byte in &mut out[i..end] {
                if *byte != b'\n' {
                    *byte = b' ';
                }
            }
            i = end;
        }
        // Only whole ASCII-delimited runs were blanked, so the bytes stay UTF-8.
        String::from_utf8(out).unwrap()
    }

    /// The braces of the first block at or after `from` in `code` (a [`code_only`] text):
    /// the offsets of its `{` and of the `}` that closes it.
    fn block_at(code: &str, from: usize) -> Option<(usize, usize)> {
        let open = from + code.get(from..)?.find('{')?;
        let mut depth = 0usize;
        for (k, byte) in code.as_bytes()[open..].iter().enumerate() {
            match byte {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some((open, open + k));
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// The body (between its braces) of the first block that follows `header` in `text`,
    /// matching braces while skipping string literals and comments; `header` is looked
    /// for in code only.
    pub fn braced_body<'a>(text: &'a str, header: &str) -> Option<&'a str> {
        let code = code_only(text);
        let at = code.find(header)? + header.len();
        let (open, close) = block_at(&code, at)?;
        Some(&text[open + 1..close])
    }

    /// Every `function <name>(<params>) { … }` in code: name, parameters and the braces
    /// of its body.
    fn functions(code: &str) -> Vec<(String, Vec<String>, (usize, usize))> {
        let mut out = Vec::new();
        let mut from = 0;
        while let Some(found) = code[from..].find("function ") {
            let start = from + found;
            from = start + "function ".len();
            if start > 0 && is_ident(code.as_bytes()[start - 1]) {
                continue;
            }
            let name_len = code.as_bytes()[from..]
                .iter()
                .take_while(|&&byte| is_ident(byte))
                .count();
            let name = &code[from..from + name_len];
            let rest = &code[from + name_len..];
            let (Some(open), Some(close)) = (rest.find('('), rest.find(')')) else {
                continue;
            };
            if name.is_empty() || !rest[..open].trim().is_empty() || close < open {
                continue;
            }
            let params = rest[open + 1..close]
                .split(',')
                .map(|param| param.trim().to_owned())
                .filter(|param| !param.is_empty())
                .collect();
            if let Some(block) = block_at(code, from + name_len + close) {
                out.push((name.to_owned(), params, block));
            }
        }
        out
    }

    /// Every `function test_<name>(…) { … }` of a QML test suite: name → body.
    pub fn qml_test_functions(qml: &str) -> BTreeMap<String, String> {
        let code = code_only(qml);
        functions(&code)
            .into_iter()
            .filter(|(name, _, _)| name.starts_with("test_"))
            .map(|(name, _, (open, close))| (name, qml[open + 1..close].to_owned()))
            .collect()
    }

    /// Whether `code` calls `Fixtures.text(<ident>)` with exactly the identifier `ident`.
    fn reads_fixture(code: &str, ident: &str) -> bool {
        code.match_indices("Fixtures.text(").any(|(at, call)| {
            code[at + call.len()..]
                .trim_start()
                .strip_prefix(ident)
                .is_some_and(|rest| rest.trim_start().starts_with(')'))
        })
    }

    /// The helpers of a QML suite that read a fixture named by one of their parameters
    /// (`function answer(launch, name, code) { … Fixtures.text(name) … }`): helper → the
    /// index of that parameter.
    pub fn fixture_wrappers(qml: &str) -> BTreeMap<String, usize> {
        let code = code_only(qml);
        functions(&code)
            .into_iter()
            .filter_map(|(name, params, (open, close))| {
                let body = &code[open + 1..close];
                let index = params.iter().position(|param| reads_fixture(body, param))?;
                Some((name, index))
            })
            .collect()
    }

    /// The top-level arguments of the call whose `(` is at `open`, as offsets into the
    /// text `code` was made from; nesting and commas are read from `code`, so neither a
    /// comma nor a bracket inside a string counts.
    fn arguments(code: &str, open: usize) -> Vec<(usize, usize)> {
        let mut args = Vec::new();
        let (mut depth, mut start) = (0usize, open + 1);
        for (k, byte) in code.as_bytes()[open..].iter().enumerate() {
            let at = open + k;
            match byte {
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        args.push((start, at));
                        return args;
                    }
                }
                b',' if depth == 1 => {
                    args.push((start, at));
                    start = at + 1;
                }
                _ => {}
            }
        }
        args
    }

    /// The fixtures a scenario body reads: the string literal given to every
    /// `Fixtures.text("…")` or `Fixtures.text('…')`, and to every call of a helper of
    /// `wrappers` at its fixture parameter, in first-seen order, each once. A call inside
    /// a comment, a method of the same name (`x.answer(…)`) and an argument that is no
    /// literal name nothing.
    pub fn fixture_names(body: &str, wrappers: &BTreeMap<String, usize>) -> Vec<String> {
        let code = code_only(body);
        let bytes = code.as_bytes();
        let mut names = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            let boundary = i == 0 || !(is_ident(bytes[i - 1]) || bytes[i - 1] == b'.');
            if !boundary || !(bytes[i].is_ascii_alphabetic() || bytes[i] == b'_') {
                i += 1;
                continue;
            }
            let len = bytes[i..]
                .iter()
                .take_while(|&&byte| is_ident(byte) || byte == b'.')
                .count();
            let callee = &code[i..i + len];
            i += len;
            let index = match callee {
                "Fixtures.text" => Some(0),
                other => wrappers.get(other).copied(),
            };
            let open = i + (bytes.len() - i - code[i..].trim_start().len());
            let (Some(index), Some(b'(')) = (index, bytes.get(open)) else {
                continue;
            };
            let Some(&(start, end)) = arguments(&code, open).get(index) else {
                continue;
            };
            let arg = body[start..end].trim();
            let quoted = arg.len() >= 2
                && matches!(arg.as_bytes()[0], b'"' | b'\'')
                && arg.as_bytes()[arg.len() - 1] == arg.as_bytes()[0];
            let name = quoted.then(|| arg[1..arg.len() - 1].to_owned());
            if let Some(name) = name.filter(|name| !names.contains(name)) {
                names.push(name);
            }
        }
        names
    }

    /// The string literals of the `PLAN_FIXTURES` array constant in a Rust source, its
    /// definition found past any use of the name, `//` lines skipped.
    pub fn plan_fixture_list(rust: &str) -> Vec<String> {
        let code = code_only(rust);
        let Some(at) = code.find("const PLAN_FIXTURES") else {
            return Vec::new();
        };
        // The type (`&[&str]`) has brackets too: the array is the first one after `=`.
        let Some(open) = code[at..]
            .find('=')
            .and_then(|eq| code[at + eq..].find('[').map(|n| at + eq + n))
        else {
            return Vec::new();
        };
        let Some(close) = code[open..].find(']').map(|n| open + n) else {
            return Vec::new();
        };
        rust[open + 1..close]
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .flat_map(|line| {
                line.split('"')
                    .skip(1)
                    .step_by(2)
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    /// The offset of each needle in `text`, each found after the end of the previous one;
    /// `Err` names the first needle that is missing, or found only before the previous.
    pub fn in_order(text: &str, needles: &[&str]) -> Result<Vec<usize>, String> {
        let mut offsets = Vec::new();
        let mut from = 0;
        for (n, needle) in needles.iter().enumerate() {
            match text[from..].find(needle) {
                Some(at) => {
                    offsets.push(from + at);
                    from += at + needle.len();
                }
                None if n > 0 && text.contains(needle) => {
                    return Err(format!("{needle:?} comes only before {:?}", needles[n - 1]));
                }
                None => return Err(format!("{needle:?} is missing")),
            }
        }
        Ok(offsets)
    }
}

/// Table-driven tests of [`widget`]'s readers, each case named.
#[cfg(test)]
mod units {
    use std::collections::BTreeMap;

    use super::widget::{
        braced_body, fixture_names, fixture_wrappers, in_order, plan_fixture_list,
        qml_test_functions,
    };

    /// Named pairs: a function and its body, or a helper and its parameter's index.
    type Pairs<V> = &'static [(&'static str, V)];
    /// A case: its name, its input, and what the reader gives for it.
    type Case<T> = (&'static str, &'static str, T);
    /// A [`fixture_names`] case: its name, the body, the helpers, the fixtures read.
    type NamesCase = (
        &'static str,
        &'static str,
        Pairs<usize>,
        &'static [&'static str],
    );
    /// An [`in_order`] case: its name, the text, the needles, the offsets or a word of the
    /// error.
    type OrderCase = (
        &'static str,
        &'static str,
        &'static [&'static str],
        Result<Vec<usize>, &'static str>,
    );

    fn strings(words: &[&str]) -> Vec<String> {
        words.iter().map(|word| (*word).to_owned()).collect()
    }

    #[test]
    fn qml_test_functions_splits_a_suite_into_named_bodies() {
        let cases: &[Case<Pairs<&str>>] = &[
            (
                "two_functions",
                "TestCase {\n  function test_a() { a() }\n  function test_b() { b() }\n}",
                &[("test_a", "a()"), ("test_b", "b()")],
            ),
            (
                "nested_braces_in_a_body",
                "function test_a() { if (x) { y() } z() }",
                &[("test_a", "if (x) { y() } z()")],
            ),
            (
                "a_brace_inside_a_string_is_ignored",
                "function test_a() { s(\"}\"); t('{') }",
                &[("test_a", "s(\"}\"); t('{')")],
            ),
            (
                "a_helper_that_is_not_a_test_is_ignored",
                "function helper(x) { return x }\nfunction test_a() { helper(1) }",
                &[("test_a", "helper(1)")],
            ),
            (
                "the_last_function_closes_before_the_testcase_brace",
                "TestCase {\n  function test_a() {\n    a()\n  }\n}\n",
                &[("test_a", "a()")],
            ),
            (
                "a_commented_out_test_is_ignored",
                "// function test_old() { }\nfunction test_a() { a() }",
                &[("test_a", "a()")],
            ),
            ("an_empty_suite_has_no_functions", "TestCase {}", &[]),
        ];
        for (case, input, expected) in cases {
            let got: BTreeMap<String, String> = qml_test_functions(input)
                .into_iter()
                .map(|(name, body)| (name, body.trim().to_owned()))
                .collect();
            let expected: BTreeMap<String, String> = expected
                .iter()
                .map(|(name, body)| ((*name).to_owned(), (*body).to_owned()))
                .collect();
            assert_eq!(
                got, expected,
                "{case}: input {input:?} gave {got:?}, expected {expected:?}"
            );
        }
    }

    #[test]
    fn fixture_names_reads_every_fixtures_text_call() {
        let wrapped = |pairs: &[(&str, usize)]| -> BTreeMap<String, usize> {
            pairs
                .iter()
                .map(|(name, index)| ((*name).to_owned(), *index))
                .collect()
        };
        let cases: &[NamesCase] = &[
            ("none", "compare(1, 1)", &[], &[]),
            (
                "one_double_quoted",
                "x(Fixtures.text(\"plan-a\"))",
                &[],
                &["plan-a"],
            ),
            (
                "one_single_quoted",
                "x(Fixtures.text('plan-a'))",
                &[],
                &["plan-a"],
            ),
            (
                "several_in_order",
                "Fixtures.text(\"b\"); Fixtures.text(\"a\")",
                &[],
                &["b", "a"],
            ),
            (
                "a_repeated_name_is_listed_once",
                "Fixtures.text(\"a\"); Fixtures.text(\"a\")",
                &[],
                &["a"],
            ),
            (
                "a_commented_out_call_is_ignored",
                "// Fixtures.text(\"a\")\nFixtures.text(\"b\")",
                &[],
                &["b"],
            ),
            (
                "a_wrapper_call_names_its_fixture",
                "showList(\"plan-list\")",
                &[("showList", 0)],
                &["plan-list"],
            ),
            (
                "a_wrapper_with_a_nested_call_before_the_name",
                "answer(updateFoo(), \"plan-x\")",
                &[("answer", 1)],
                &["plan-x"],
            ),
            (
                "a_method_of_the_same_name_is_no_wrapper",
                "service.answer(a, \"plan-x\")",
                &[("answer", 1)],
                &[],
            ),
            (
                "a_variable_argument_names_nothing",
                "showList(name)",
                &[("showList", 0)],
                &[],
            ),
        ];
        for (case, input, wrappers, expected) in cases {
            let got = fixture_names(input, &wrapped(wrappers));
            assert_eq!(
                got,
                strings(expected),
                "{case}: input {input:?} with helpers {wrappers:?} gave {got:?}, expected {expected:?}"
            );
        }
    }

    #[test]
    fn fixture_wrappers_finds_the_helpers_that_read_a_named_fixture() {
        let cases: &[Case<Pairs<usize>>] = &[
            ("none", "function f() {}", &[]),
            (
                "a_helper_reading_its_parameter",
                "function answer(launch, name, code) { launch.finish(code || 0, Fixtures.text(name)) }",
                &[("answer", 1)],
            ),
            (
                "a_helper_reading_a_literal_is_no_wrapper",
                "function f(x) { return Fixtures.text(\"a\") }",
                &[],
            ),
            (
                "two_helpers",
                "function showList(name) { s = Model.reduceList(JSON.parse(Fixtures.text(name))) }\n\
                 function planIdOf(name) { return JSON.parse(Fixtures.text( name )).data.plan_id }",
                &[("planIdOf", 0), ("showList", 0)],
            ),
        ];
        for (case, input, expected) in cases {
            let got = fixture_wrappers(input);
            let expected: BTreeMap<String, usize> = expected
                .iter()
                .map(|(name, index)| ((*name).to_owned(), *index))
                .collect();
            assert_eq!(
                got, expected,
                "{case}: input {input:?} gave {got:?}, expected {expected:?}"
            );
        }
    }

    #[test]
    fn plan_fixture_list_parses_the_rust_constant() {
        let cases: &[(&str, &str, &[&str])] = &[
            (
                "one_line",
                "pub const PLAN_FIXTURES: &[&str] = &[\"a\", \"b\"];",
                &["a", "b"],
            ),
            (
                "several_lines_with_a_trailing_comma",
                "const PLAN_FIXTURES: &[&str] = &[\n    \"a\",\n    \"b\",\n];",
                &["a", "b"],
            ),
            (
                "a_comment_line_inside",
                "const PLAN_FIXTURES: &[&str] = &[\n    \"a\",\n    // \"old\",\n    \"b\",\n];",
                &["a", "b"],
            ),
            (
                "a_use_before_the_definition_is_skipped",
                "f(PLAN_FIXTURES.contains(\"z\"));\nconst PLAN_FIXTURES: &[&str] = &[\"a\"];",
                &["a"],
            ),
            (
                "absent_constant_gives_nothing",
                "const OTHER: &[&str] = &[\"a\"];",
                &[],
            ),
        ];
        for (case, input, expected) in cases {
            let got = plan_fixture_list(input);
            assert_eq!(
                got,
                strings(expected),
                "{case}: input {input:?} gave {got:?}, expected {expected:?}"
            );
        }
    }

    #[test]
    fn in_order_finds_needles_in_sequence() {
        let cases: &[OrderCase] = &[
            ("all_in_order", "a b c", &["a", "c"], Ok(vec![0, 4])),
            ("one_missing", "a b", &["a", "z"], Err("missing")),
            ("two_out_of_order", "b a", &["a", "b"], Err("before")),
            (
                "a_needle_repeated_later_counts_after_the_previous",
                "b a b",
                &["a", "b"],
                Ok(vec![2, 4]),
            ),
        ];
        for (case, text, needles, expected) in cases {
            let got = in_order(text, needles);
            let fits = match (&got, expected) {
                (Ok(offsets), Ok(want)) => offsets == want,
                (Err(message), Err(word)) => message.contains(word),
                _ => false,
            };
            assert!(
                fits,
                "{case}: input {text:?} {needles:?} gave {got:?}, expected {expected:?}"
            );
        }
    }

    #[test]
    fn braced_body_skips_strings_and_comments() {
        let cases: &[(&str, &str, &str, Option<&str>)] = &[
            (
                "the_first_block_after_the_header",
                "fn a() { x }\nfn b() { y }",
                "fn b(",
                Some("y"),
            ),
            (
                "a_brace_in_a_comment_or_string_is_ignored",
                "fn b() {\n    // }\n    s(\"}\");\n    t('}');\n}",
                "fn b(",
                Some("// }\n    s(\"}\");\n    t('}');"),
            ),
            (
                "a_header_in_a_comment_is_not_the_header",
                "// fn b() { no }\nfn b() { yes }",
                "fn b(",
                Some("yes"),
            ),
            ("an_absent_header_gives_nothing", "fn a() {}", "fn b(", None),
        ];
        for (case, text, header, expected) in cases {
            let got = braced_body(text, header).map(str::trim);
            assert_eq!(
                got, *expected,
                "{case}: input {text:?} after {header:?} gave {got:?}, expected {expected:?}"
            );
        }
    }
}

#[cfg(test)]
mod support {
    use std::collections::BTreeMap;
    use std::fs;
    use std::io::ErrorKind;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    use omaddons::app::precondition::preview_argv;
    use serde_json::{Value, json};
    use sha2::{Digest, Sha256};

    use crate::common::home::Home;
    use crate::common::landing::{Landing, Source};
    use crate::common::memo;
    use crate::common::netlog::{NetLogView, net_log};
    use crate::common::pause::Pause;
    use crate::common::plans;
    use crate::common::tlog::{Ran, TestLog};

    /// A home and the landing that serves its addon.
    pub struct Demo {
        pub home: Home,
        pub landing: Landing,
    }

    /// Step "build": release 1 of `source`'s addon installed on each of `targets`, then
    /// release 2 published as the newest and the release memo a day old, so the next
    /// `update` asks the source again.
    pub fn behind(log: &TestLog, products: &[&str], targets: &[&str], source: Source) -> Demo {
        log.step("build");
        let mut home = Home::new(products);
        log.env(&home);
        let landing = Landing::start(&mut home, source);
        let mut demo = Demo { home, landing };
        for &target in targets {
            let r#ref = demo.landing.r#ref();
            log.run(&demo.home, &["install", r#ref, "-t", target, "--json"])
                .ok();
            assert_release(log, &demo, target, 1);
        }
        let version = demo.landing.land(&mut demo.home);
        memo::age(&demo.home, 24);
        log.note(format!(
            "landed {} release 2 ({version}); the release memo is a day old",
            source.label()
        ));
        demo
    }

    /// The client folder of a target id the demo homes have.
    pub fn folder(target: &str) -> &'static str {
        match target {
            "retail" => "_retail_",
            "classic" => "_classic_",
            other => panic!("no client folder is known for target {other}"), // ubs:ignore — a builder bug
        }
    }

    /// The landing's marker for release `n` is on `target`'s AddOns, with its content.
    #[track_caller]
    pub fn assert_release(log: &TestLog, demo: &Demo, target: &str, n: u32) {
        let Some((path, expected)) = demo.landing.marker(n) else {
            let source = demo.landing.source().label();
            panic!("the {source} landing has no marker for release {n}"); // ubs:ignore — a builder bug
        };
        let file = demo.home.addons(folder(target)).join(&path);
        let actual = match fs::read_to_string(&file) {
            Ok(text) => text,
            Err(error) if error.kind() == ErrorKind::NotFound => String::new(),
            Err(error) => panic!("read {}: {error}", file.display()), // ubs:ignore — the assertion
        };
        log.assert_eq(
            &format!("{target}: {path} is release {n}"),
            &actual,
            &expected,
        );
    }

    /// What a refused run must leave as it was: each target's AddOns and the records (as
    /// trees in the log), history's length and every plan file's digest.
    pub struct Baseline {
        pub history: usize,
        pub plans: BTreeMap<String, String>,
    }

    pub fn baseline(log: &TestLog, home: &Home, targets: &[&str]) -> Baseline {
        for target in targets {
            log.tree(&format!("addons {target}"), &home.addons(folder(target)));
        }
        log.tree("records", &home.state().join("targets"));
        let baseline = Baseline {
            history: history_lines(home),
            plans: plan_digests(home),
        };
        log.json(
            "baseline",
            &json!({"history_lines": baseline.history, "plan_files": baseline.plans}),
        );
        baseline
    }

    /// Each target's AddOns, the records and history's length are as `before` found them
    /// (a dry run may still store its plan).
    #[track_caller]
    pub fn assert_untouched(log: &TestLog, home: &Home, targets: &[&str], before: &Baseline) {
        for target in targets {
            log.assert_unchanged(&format!("addons {target}"), &home.addons(folder(target)));
        }
        log.assert_unchanged("records", &home.state().join("targets"));
        log.assert_eq("history lines", &history_lines(home), &before.history);
    }

    /// [`assert_untouched`], and every plan file is byte for byte as it was.
    #[track_caller]
    pub fn assert_nothing_changed(log: &TestLog, home: &Home, targets: &[&str], before: &Baseline) {
        assert_untouched(log, home, targets, before);
        log.assert_eq("plan files", &plan_digests(home), &before.plans);
    }

    /// The non-empty lines of `<state>/omaddons/history.jsonl`; 0 when it is absent.
    pub fn history_lines(home: &Home) -> usize {
        let path = home.state().join("history.jsonl");
        match fs::read_to_string(&path) {
            Ok(text) => text.lines().filter(|line| !line.trim().is_empty()).count(),
            Err(error) if error.kind() == ErrorKind::NotFound => 0,
            Err(error) => panic!("read {}: {error}", path.display()), // ubs:ignore — the assertion
        }
    }

    /// Each plan file's name and the SHA-256 of its bytes (leases left out).
    pub fn plan_digests(home: &Home) -> BTreeMap<String, String> {
        plans::plan_files(&home.env)
            .iter()
            .map(|path| {
                let name = path.file_name().unwrap().to_string_lossy().into_owned();
                (name, sha256(&fs::read(path).unwrap()))
            })
            .collect()
    }

    pub fn sha256(bytes: &[u8]) -> String {
        Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    /// The `net.log` entries after the first `before`, logged before anything counts them.
    pub fn requests(log: &TestLog, home: &Home, before: usize) -> NetLogView {
        let mut view = net_log(home);
        view.entries = view.entries.split_off(before);
        log.note(format!(
            "{} requests since the mark:\n{}",
            view.len(),
            view.context()
        ));
        view
    }

    /// The fix a lifecycle or selection refusal of `plan` carries: its preview again.
    pub fn expected_fix(home: &Home, plan: &str) -> Vec<String> {
        let display = plans::read_plan(&home.env, plan)["display_argv"].clone();
        let display: Vec<String> = serde_json::from_value(display).unwrap();
        preview_argv(&display)
    }

    /// `plan_stale`, exit 1 and not retryable: the refusal's `details`, the error logged.
    #[track_caller]
    pub fn stale(log: &TestLog, ran: &Ran) -> Value {
        ran.fails(1, "plan_stale");
        let error = ran.envelope()["error"].clone();
        log.json("refusal", &error);
        log.assert_eq("retryable", &error["retryable"], &json!(false));
        error["details"].clone()
    }

    /// `commands[]` offers exactly `omaddons apply <id>`.
    pub fn apply_hint(ran: &Ran, id: &str) -> bool {
        ran.commands()
            .iter()
            .any(|command| command.action == "apply" && command.argv == ["omaddons", "apply", id])
    }

    /// `data.outcomes[]` as `(target, status, tx)`, logged.
    pub fn outcomes(log: &TestLog, ran: &Ran) -> Vec<(String, String, Option<String>)> {
        let text = |value: &Value| value.as_str().unwrap_or_default().to_owned();
        let outcomes: Vec<_> = ran.data()["outcomes"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|outcome| {
                (
                    text(&outcome["target"]),
                    text(&outcome["status"]),
                    outcome["tx"].as_str().map(str::to_owned),
                )
            })
            .collect();
        log.json("outcomes", &outcomes);
        outcomes
    }

    /// The one change `approval.selection` holds; the test fails when it holds another
    /// number.
    #[track_caller]
    pub fn selected(log: &TestLog, approval: &Value) -> Value {
        let selection = approval["selection"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        log.assert_eq("changes approved", &selection.len(), &1);
        selection[0].clone()
    }

    /// Every regular file under `dir`, symlinks not followed; none when `dir` is absent.
    pub fn regular_files(dir: &Path, out: &mut Vec<PathBuf>) {
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == ErrorKind::NotFound => return,
            Err(error) => panic!("list {}: {error}", dir.display()), // ubs:ignore — the assertion
        };
        for entry in entries {
            let path = entry.unwrap().path();
            let kind = fs::symlink_metadata(&path).unwrap().file_type();
            if kind.is_dir() {
                regular_files(&path, out);
            } else if kind.is_file() {
                out.push(path);
            }
        }
    }

    /// `path`, relative to the repository root.
    pub fn repo(path: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join(path)
    }

    /// Whether the JSON array `list` holds the string `word`.
    pub fn contains(list: &Value, word: &str) -> bool {
        list.as_array()
            .is_some_and(|items| items.iter().any(|item| item == word))
    }

    /// The names in `dir`, sorted; none when it is absent.
    pub fn names(dir: &Path) -> Vec<String> {
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == ErrorKind::NotFound => return Vec::new(),
            Err(error) => panic!("list {}: {error}", dir.display()), // ubs:ignore — the assertion
        };
        let mut names: Vec<String> = entries
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// Hit after a bound run's own planning, its first token check and its staging, just
    /// before it takes the global lock: an edit made while it waits there is "late".
    pub const PRESTAGED: &str = "engine.snapshot.prestaged";
    /// Hit once, when every target is planned and before any commit: an applier that
    /// slipped past the plan's lease would plan, and stop here.
    pub const PLANNED: &str = "engine.plan.done";
    /// Hit after each target's commit window has unlocked and cleaned up, before the next
    /// target starts: a pause there holds no global lock.
    pub const CLEANUP: &str = "engine.cleanup.done";

    /// The waiting applier reached neither sentinel ([`PLANNED`], [`CLEANUP`]). When it
    /// did, it slipped past the lease: both are resumed, so it exits, and the test fails.
    #[track_caller]
    pub fn never_planned(log: &TestLog, waiter: &Pause) {
        let reached: Vec<(&str, bool)> = [PLANNED, CLEANUP]
            .map(|name| (name, waiter.was_reached(name)))
            .to_vec();
        log.note(format!("the waiting applier's markers: {reached:?}"));
        let slipped = reached.iter().any(|(_, at)| *at);
        if slipped {
            for name in [PLANNED, CLEANUP] {
                log.resume(waiter, name);
            }
        }
        log.assert_eq(
            "the second applier planned or committed while the first held the lease",
            &slipped,
            &false,
        );
    }

    /// Whether process `pid` has ended: gone, or a zombie not reaped yet.
    fn exited(pid: u32) -> bool {
        match fs::read_to_string(format!("/proc/{pid}/stat")) {
            Err(_) => true,
            Ok(stat) => stat
                .rsplit_once(')')
                .and_then(|(_, rest)| rest.split_whitespace().next())
                .is_none_or(|state| matches!(state, "Z" | "X")),
        }
    }

    /// Watches the waiting applier `pid` until it ends (30 s at most), checking its
    /// sentinels on every poll, so one that slipped past the lease fails the test at once
    /// instead of sitting paused.
    #[track_caller]
    pub fn until_it_ends(log: &TestLog, waiter: &Pause, pid: u32) {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if [PLANNED, CLEANUP]
                .iter()
                .any(|name| waiter.was_reached(name))
            {
                never_planned(log, waiter);
            }
            if exited(pid) {
                log.note(format!("pid {pid} ended"));
                return;
            }
            if Instant::now() >= deadline {
                log.note(format!("pid {pid} still runs after 30 s"));
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

/// A fresh install and an update, each previewed with `--dry-run` and applied through its
/// stored plan; the human preview names the same plan, `apply --dry-run` re-checks it
/// without changing anything, and history records the approval the update ran.
#[test]
fn demo_install_then_update_preview_then_apply() {
    let log = tlog!();
    log.step("build");
    let mut home = Home::new(&["wow"]);
    log.env(&home);
    let landing = Landing::start(&mut home, Source::GitHub);
    let mut demo = Demo { home, landing };

    log.step("preview the install");
    let preview = log.run(
        &demo.home,
        &["install", "gh:o/foo", "-t", "retail", "--dry-run", "--json"],
    );
    preview.ok();
    let p1 = plans::plan_id(&preview);
    let approval = preview.data()["approval"].clone();
    log.json("approval", &approval);
    let item = selected(&log, &approval);
    log.assert_eq(
        "an install of release 1 on retail",
        &(&item["op"], &item["target_id"], &item["to_version"]),
        &(
            &json!("install"),
            &json!("retail"),
            &json!(demo.landing.version(1)),
        ),
    );
    log.assert_eq(
        "commands[] offers its apply",
        &apply_hint(&preview, &p1),
        &true,
    );
    let file = plans::plans_dir(&demo.home.env).join(format!("{p1}.json"));
    let mode = fs::metadata(&file)
        .ok()
        .map(|meta| format!("{:o}", meta.permissions().mode() & 0o777));
    log.assert_eq("the plan file's mode", &mode, &Some("600".to_owned()));
    log.assert_eq(
        "nothing installed yet",
        &demo.home.addons("_retail_").join("Foo").exists(),
        &false,
    );

    log.step("apply the install");
    let applied = log.run(&demo.home, &["apply", &p1, "--json"]);
    applied.ok();
    log.assert_eq(
        "the applied command",
        &applied.data()["applied"]["command"],
        &json!("install"),
    );
    let done = outcomes(&log, &applied);
    let committed: Vec<_> = done
        .iter()
        .map(|(target, status, tx)| (target.as_str(), status.as_str(), tx.is_some()))
        .collect();
    log.assert_eq(
        "committed on retail",
        &committed,
        &vec![("retail", "committed", true)],
    );
    assert_release(&log, &demo, "retail", 1);

    log.step("a release lands");
    let version = demo.landing.land(&mut demo.home);
    memo::age(&demo.home, 24);
    log.note(format!(
        "landed github release 2 ({version}); the release memo is a day old"
    ));

    log.step("preview the update");
    let before = baseline(&log, &demo.home, &["retail"]);
    let preview = log.run(
        &demo.home,
        &["update", "foo", "-t", "retail", "--dry-run", "--json"],
    );
    preview.ok();
    let p2 = plans::plan_id(&preview);
    let approval = preview.data()["approval"].clone();
    log.json("approval", &approval);
    let item = selected(&log, &approval);
    log.assert_eq(
        "an update from release 1 to release 2",
        &(&item["op"], &item["from_version"], &item["to_version"]),
        &(
            &json!("update"),
            &json!(demo.landing.version(1)),
            &json!(demo.landing.version(2)),
        ),
    );
    log.assert_eq(
        "routine, with no reasons",
        &(&approval["routine"], &approval["reasons"]),
        &(&json!(true), &json!([])),
    );
    log.assert_eq(
        "commands[] offers its apply",
        &apply_hint(&preview, &p2),
        &true,
    );
    let plan = plans::read_plan(&demo.home.env, &p2);
    log.json("plan", &plan);
    assert_untouched(&log, &demo.home, &["retail"], &before);

    log.step("the human preview names the same plan");
    let human = log.run(&demo.home, &["update", "foo", "-t", "retail", "--dry-run"]);
    human.exit(0);
    let line = format!("Plan {p2} stored for 24 hours.");
    log.note(format!("looking for: {line}"));
    log.assert_eq(
        "stdout names the stored plan",
        &human.stdout().contains(&line),
        &true,
    );
    let stored: BTreeSet<String> = plans::plan_files(&demo.home.env)
        .iter()
        .map(|path| path.file_stem().unwrap().to_string_lossy().into_owned())
        .collect();
    log.assert_eq(
        "one plan file each for P1 and P2 (the identical preview reused P2)",
        &stored,
        &BTreeSet::from([p1.clone(), p2.clone()]),
    );

    log.step("re-check the plan without changing anything");
    let before = baseline(&log, &demo.home, &["retail"]);
    let check = log.run(&demo.home, &["apply", &p2, "--dry-run", "--json"]);
    check.ok();
    log.assert_eq(
        "approved is the preview's approval",
        &check.data()["approved"],
        &approval,
    );
    log.assert_eq(
        "the result is a dry run",
        &check.data()["result"]["dry_run"],
        &json!(true),
    );
    log.assert_eq(
        "every outcome unchanged, with no transaction",
        &outcomes(&log, &check),
        &vec![("retail".to_owned(), "unchanged".to_owned(), None)],
    );
    assert_nothing_changed(&log, &demo.home, &["retail"], &before);
    log.assert_eq(
        "no lease was taken",
        &plans::lease_path(&demo.home.env, &p2).exists(),
        &false,
    );

    log.step("apply the update");
    let mark = net_log(&demo.home).len();
    let applied = log.run(&demo.home, &["apply", &p2, "--json"]);
    applied.ok();
    log.assert_eq(
        "the applied command and argv",
        &(
            &applied.data()["applied"]["command"],
            &applied.data()["applied"]["display_argv"],
        ),
        &(&json!("update"), &plan["display_argv"]),
    );
    let done = outcomes(&log, &applied);
    log.assert_eq("one outcome", &done.len(), &1);
    let (target, status, tx2) = done[0].clone();
    log.assert_eq(
        "committed on retail",
        &(target.as_str(), status.as_str(), tx2.is_some()),
        &("retail", "committed", true),
    );
    let tx2 = tx2.unwrap_or_default();
    assert_release(&log, &demo, "retail", 2);
    let cost = requests(&log, &demo.home, mark);
    log.assert_eq(
        "one forced revalidation, and the archive the preview downloaded",
        &(
            cost.count_purpose("release-latest"),
            cost.count_purpose("asset"),
        ),
        &(1, 0),
    );
    let undo = applied
        .commands()
        .iter()
        .any(|command| command.action == "undo" && command.argv.contains(&tx2));
    log.assert_eq("commands[] offers the update's undo", &undo, &true);

    log.step("history records the approval");
    let history = log.run(&demo.home, &["history", "--json", "--limit", "5"]);
    history.ok();
    let entry = history.data()["entries"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|entry| entry["tx"] == json!(tx2))
        .cloned()
        .unwrap_or(Value::Null);
    log.json("the update's history line", &entry);
    log.assert_eq(
        "it carries the plan and its display_argv",
        &(&entry["plan"], &entry["argv"]),
        &(&json!(p2), &plan["display_argv"]),
    );
    log.assert_eq(
        "history lines carrying P2",
        &plans::history_txs_for(&log, &demo.home, &p2),
        &vec![("retail".to_owned(), tx2.clone())],
    );
}

/// A release published after the preview makes `apply` stale (`selection`), never a
/// different install; the stale `apply` refreshed the memo and cached the new archive, so
/// the re-preview shows the new release without asking or downloading, and its plan
/// installs exactly that release.
#[test]
fn demo_a_release_landing_in_between_is_plan_stale_and_the_re_preview_shows_it() {
    let log = tlog!();
    let mut demo = behind(&log, &["wow"], &["retail"], Source::GitHub);

    log.step("preview");
    let preview = log.run(
        &demo.home,
        &["update", "foo", "-t", "retail", "--dry-run", "--json"],
    );
    preview.ok();
    let p = plans::plan_id(&preview);
    let item = selected(&log, &preview.data()["approval"]);
    log.assert_eq(
        "to release 2",
        &item["to_version"],
        &json!(demo.landing.version(2)),
    );

    log.step("a release lands after the preview");
    // Not aged: the landing is inside the 15-minute window, where only `apply`'s forced
    // revalidation can see it.
    let version = demo.landing.land(&mut demo.home);
    log.note(format!(
        "landed github release 3 ({version}) inside the window"
    ));

    log.step("apply the stale plan");
    let before = baseline(&log, &demo.home, &["retail"]);
    let mark = net_log(&demo.home).len();
    let ran = log.run(&demo.home, &["apply", &p, "--json"]);
    let details = stale(&log, &ran);
    log.assert_eq(
        "details.changed",
        &details["changed"],
        &json!(["selection"]),
    );
    log.assert_eq(
        "the fix previews the plan again",
        &ran.envelope()["error"]["fix_argv"],
        &json!(expected_fix(&demo.home, &p)),
    );
    assert_nothing_changed(&log, &demo.home, &["retail"], &before);
    assert_release(&log, &demo, "retail", 1);
    let cost = requests(&log, &demo.home, mark);
    log.assert_eq(
        "apply revalidated",
        &cost.count_purpose("release-latest"),
        &1,
    );

    log.step("the re-preview shows the new release");
    let mark = net_log(&demo.home).len();
    let again = log.run(
        &demo.home,
        &["update", "foo", "-t", "retail", "--dry-run", "--json"],
    );
    again.ok();
    let p3 = plans::plan_id(&again);
    // ubs:ignore — comparing plan ids, not secrets
    log.assert_eq("a new plan", &(p3 != p), &true);
    let item = selected(&log, &again.data()["approval"]);
    log.assert_eq(
        "to release 3",
        &item["to_version"],
        &json!(demo.landing.version(3)),
    );
    let cost = requests(&log, &demo.home, mark);
    log.assert_eq(
        "the memo and the cache the stale apply filled answered",
        &(
            cost.count_purpose("release-latest"),
            cost.count_purpose("asset"),
        ),
        &(0, 0),
    );

    log.step("apply the new plan");
    let applied = log.run(&demo.home, &["apply", &p3, "--json"]);
    applied.ok();
    let statuses: Vec<_> = outcomes(&log, &applied)
        .into_iter()
        .map(|(target, status, tx)| (target, status, tx.is_some()))
        .collect();
    log.assert_eq(
        "committed on retail",
        &statuses,
        &vec![("retail".to_owned(), "committed".to_owned(), true)],
    );
    assert_release(&log, &demo, "retail", 3);
    let lines = plans::history_txs_for(&log, &demo.home, &p3);
    let targets: Vec<&str> = lines.iter().map(|(target, _)| target.as_str()).collect();
    log.assert_eq("one history line, on retail", &targets, &vec!["retail"]);
}

/// A plan history records as applied is refused as `already_applied`, naming its
/// transaction, also an hour past its expiry; `plans show` agrees it completed.
#[test]
fn demo_a_second_apply_is_already_applied() {
    let log = tlog!();
    let demo = behind(&log, &["wow"], &["retail"], Source::GitHub);

    log.step("preview and apply");
    let preview = log.run(
        &demo.home,
        &["update", "foo", "-t", "retail", "--dry-run", "--json"],
    );
    preview.ok();
    let p = plans::plan_id(&preview);
    let applied = log.run(&demo.home, &["apply", &p, "--json"]);
    applied.ok();
    let done = outcomes(&log, &applied);
    let statuses: Vec<_> = done
        .iter()
        .map(|(target, status, tx)| (target.as_str(), status.as_str(), tx.is_some()))
        .collect();
    log.assert_eq(
        "committed on retail",
        &statuses,
        &vec![("retail", "committed", true)],
    );
    let tx = done[0].2.clone().unwrap_or_default();

    log.step("apply again");
    let before = baseline(&log, &demo.home, &["retail"]);
    let again = log.run(&demo.home, &["apply", &p, "--json"]);
    let details = stale(&log, &again);
    log.assert_eq(
        "already applied, naming its transaction",
        &(&details["reason"], &details["txs"]),
        &(&json!("already_applied"), &json!([tx])),
    );
    log.assert_eq(
        "the fix previews the plan again",
        &again.envelope()["error"]["fix_argv"],
        &json!(expected_fix(&demo.home, &p)),
    );
    assert_nothing_changed(&log, &demo.home, &["retail"], &before);
    assert_release(&log, &demo, "retail", 2);

    log.step("a day later it is still already applied");
    let expires = plans::read_plan(&demo.home.env, &p)["expires_at"]
        .as_str()
        .unwrap_or_default()
        .parse::<Timestamp>()
        .unwrap();
    let later = expires
        .checked_add(SignedDuration::from_hours(1))
        .unwrap()
        .to_string();
    log.note(format!(
        "the plan expires at {expires}; the clock reads {later}"
    ));
    let late = log
        .cmd(&demo.home, &["apply", &p, "--json"])
        .env("OMADDONS_NOW", &later)
        .run();
    let details = stale(&log, &late);
    log.assert_eq(
        "already_applied is checked before expiry",
        &details["reason"],
        &json!("already_applied"),
    );
    assert_nothing_changed(&log, &demo.home, &["retail"], &before);

    log.step("plans show agrees");
    let shown = plans::show(&log, &demo.home, &[&p], &[]);
    shown.ok();
    log.assert_eq(
        "completed, with the transaction",
        &(&shown.data()["state"], plans::evidence_txs(&shown)),
        &(&json!("completed"), vec![("retail".to_owned(), tx)]),
    );
}

/// A Tukui plan whose preview downloaded the archive applies from the blob cache: its
/// stored release has no URL, `apply` asks for the list only and resolves no download
/// link, and no download token is printed or kept anywhere.
#[test]
fn demo_a_tukui_plan_applies_from_the_cache() {
    let log = tlog!();
    log.step("build");
    let mut home = Home::new(&["wow"]);
    log.env(&home);
    let landing = Landing::start(&mut home, Source::Tukui);
    let demo = Demo { home, landing };
    let tokens = demo.landing.download_tokens(1);
    log.json("the landing's download tokens (synthetic)", &tokens);
    let mut seen: Vec<(String, String)> = Vec::new();

    log.step("preview downloads and caches the archive");
    let mark = net_log(&demo.home).len();
    let preview = log.run(
        &demo.home,
        &[
            "install",
            "tukui:elvui",
            "-t",
            "retail",
            "--dry-run",
            "--json",
        ],
    );
    preview.ok();
    seen.push(("preview stdout".to_owned(), preview.stdout().to_owned()));
    seen.push(("preview stderr".to_owned(), preview.stderr().to_owned()));
    let p = plans::plan_id(&preview);
    let cost = requests(&log, &demo.home, mark);
    log.assert_eq(
        "the list, a resolve, and one download",
        &(
            cost.count_purpose("tukui-list") >= 1,
            cost.count_purpose("tukui-addon") >= 1,
            cost.count_purpose("asset"),
        ),
        &(true, true, 1),
    );
    let releases = plans::read_plan(&demo.home.env, &p)["releases"].clone();
    log.json("plan releases", &releases);
    let stored: Vec<_> = releases
        .as_array()
        .into_iter()
        .flatten()
        .map(|release| {
            (
                release["source"].clone(),
                release["key"].clone(),
                release["url"].clone(),
            )
        })
        .collect();
    log.assert_eq(
        "one deferred Tukui release, stored without its URL",
        &stored,
        &vec![(json!("tukui"), json!("elvui"), Value::Null)],
    );

    log.step("apply from the cache");
    let mark = net_log(&demo.home).len();
    // `-v` makes the executed install report `asset_url`, so its `null` says that a
    // cache hit resolved no link, not that the key was left out.
    let applied = log.run(&demo.home, &["apply", &p, "--json", "-v"]);
    applied.ok();
    seen.push(("apply stdout".to_owned(), applied.stdout().to_owned()));
    seen.push(("apply stderr".to_owned(), applied.stderr().to_owned()));
    let statuses: Vec<_> = outcomes(&log, &applied)
        .into_iter()
        .map(|(target, status, tx)| (target, status, tx.is_some()))
        .collect();
    log.assert_eq(
        "committed on retail",
        &statuses,
        &vec![("retail".to_owned(), "committed".to_owned(), true)],
    );
    let item = &applied.data()["result"]["items"][0];
    log.json("the applied item", item);
    log.assert_eq(
        "-v in effect, and a cache hit resolves no URL",
        &(
            item.get("optional_dependencies").is_some(),
            &item["asset_url"],
        ),
        &(true, &Value::Null),
    );
    let cost = requests(&log, &demo.home, mark);
    log.assert_eq(
        "the list alone: no resolve, no download",
        &(
            cost.count_purpose("tukui-list"),
            cost.count_purpose("tukui-addon"),
            cost.count_purpose("asset"),
        ),
        &(1, 0, 0),
    );
    assert_release(&log, &demo, "retail", 1);

    log.step("no download token is kept or printed");
    let mut files = Vec::new();
    for dir in [
        demo.home.env.state(),
        demo.home.env.cache(),
        demo.home.env.config(),
    ] {
        regular_files(&dir, &mut files);
    }
    let net_log_searched = files
        .iter()
        .any(|path| path.file_name().is_some_and(|name| name == "net.log"));
    for path in &files {
        let bytes = fs::read(path).unwrap();
        let text = String::from_utf8_lossy(&bytes).into_owned();
        seen.push((path.display().to_string(), text));
    }
    log.note(format!(
        "searched {} files under <state>, <cache> and <config>, and 4 output streams",
        files.len()
    ));
    log.assert_eq("net.log was searched", &net_log_searched, &true);
    let leaked: Vec<&str> = seen
        .iter()
        .filter(|(_, text)| tokens.iter().any(|token| text.contains(token.as_str())))
        .map(|(label, _)| label.as_str())
        .collect();
    log.assert_eq("no download token", &leaked, &Vec::<&str>::new());
}

/// A file a person edits after `apply`'s own first check, while it waits for the commit
/// lock, is caught under the lock: `plan_stale` naming `observed`, nothing moved, the edit
/// kept. The re-preview says it replaces local changes, its plan commits knowingly, and
/// the undo brings the edit back byte for byte.
#[test]
fn demo_a_late_hand_edit_is_caught_under_the_lock() {
    let log = tlog!();
    let demo = behind(&log, &["wow"], &["retail"], Source::GitHub);

    log.step("preview");
    let preview = log.run(
        &demo.home,
        &["update", "foo", "-t", "retail", "--dry-run", "--json"],
    );
    preview.ok();
    let p = plans::plan_id(&preview);
    let approval = &preview.data()["approval"];
    log.assert_eq(
        "routine, with no reasons",
        &(&approval["routine"], &approval["reasons"]),
        &(&json!(true), &json!([])),
    );
    let lease = plans::lease_path(&demo.home.env, &p);

    log.step("apply pauses before the commit lock");
    let pause = Pause::new(demo.home.env.root());
    let paused = log
        .cmd(&demo.home, &["apply", &p, "--json"])
        .failpoints(&format!("{PRESTAGED}=pause"))
        .pause(&pause)
        .spawn();
    log.reached(&pause, PRESTAGED);
    log.note(
        "apply passed its lifecycle checks, re-planned bound to the stored token and staged \
         release 2; it holds the plan's lease and no global lock",
    );
    log.assert_eq(
        "the paused apply holds the plan's lease",
        &plans::holds_open(paused.pid(), &lease),
        &true,
    );

    log.step("a hand edit after the first check");
    let core = demo.home.addons("_retail_").join("Foo/core.lua");
    fs::OpenOptions::new()
        .append(true)
        .open(&core)
        .unwrap()
        .write_all(b"-- edited by hand after the preview\n")
        .unwrap();
    let edited = fs::read(&core).unwrap();
    let before = baseline(&log, &demo.home, &["retail"]);

    log.step("the commit refuses under the lock");
    log.resume(&pause, PRESTAGED);
    let ran = paused.wait();
    let details = stale(&log, &ran);
    log.assert_eq(
        "details.changed holds observed",
        &contains(&details["changed"], "observed"),
        &true,
    );
    let error = &ran.envelope()["error"];
    log.assert_eq(
        "the fix previews the plan again",
        &error["fix_argv"],
        &json!(expected_fix(&demo.home, &p)),
    );
    // The witness names a folder by its key, the case-folded name (`src/engine/witness.rs`).
    log.assert_eq(
        "the message names Foo as what changed under the lock",
        &error["message"]
            .as_str()
            .unwrap_or_default()
            .contains("(observed changed: foo changed after the preview)"),
        &true,
    );
    assert_nothing_changed(&log, &demo.home, &["retail"], &before);
    log.assert_eq(
        "core.lua is as the edit left it",
        &sha256(&fs::read(&core).unwrap()),
        &sha256(&edited),
    );
    let stage = demo
        .home
        .client("_retail_")
        .join("Interface/.omaddons/stage");
    log.assert_eq(
        "no journal or stage is left",
        &(names(&demo.home.state().join("journal")), names(&stage)),
        &(Vec::new(), Vec::new()),
    );
    log.assert_eq(
        "no history line carries the plan",
        &plans::history_txs_for(&log, &demo.home, &p),
        &Vec::new(),
    );

    log.step("the re-preview shows the edit");
    let again = log.run(
        &demo.home,
        &["update", "foo", "-t", "retail", "--dry-run", "--json"],
    );
    again.ok();
    let p2 = plans::plan_id(&again);
    // ubs:ignore — comparing plan ids, not secrets
    log.assert_eq("a new plan", &(p2 != p), &true);
    let approval = again.data()["approval"].clone();
    log.json("approval", &approval);
    let replaced = &approval["scope"]["local_changes_replaced"];
    log.assert_eq(
        "not routine: it replaces local changes in Foo",
        &(
            &approval["routine"],
            contains(&approval["reasons"], "local_changes_replaced"),
            replaced["count"].as_u64().is_some_and(|count| count >= 1),
            contains(&replaced["folders"], "Foo"),
        ),
        &(&json!(false), true, true, true),
    );

    log.step("apply the new plan");
    let applied = log.run(&demo.home, &["apply", &p2, "--json"]);
    applied.ok();
    let done = outcomes(&log, &applied);
    let statuses: Vec<_> = done
        .iter()
        .map(|(target, status, tx)| (target.as_str(), status.as_str(), tx.is_some()))
        .collect();
    log.assert_eq(
        "committed on retail",
        &statuses,
        &vec![("retail", "committed", true)],
    );
    let tx = done[0].2.clone().unwrap_or_default();
    assert_release(&log, &demo, "retail", 2);

    log.step("undo brings the edit back");
    log.run(&demo.home, &["undo", &tx, "--json"]).ok();
    log.assert_eq(
        "core.lua is the hand-edited file, byte for byte",
        &sha256(&fs::read(&core).unwrap()),
        &sha256(&edited),
    );
}

/// Two `apply`s of one two-client plan, the first paused between its clients' commits:
/// the second waits on the plan's lease without planning, then reports the first one's
/// work as `already_applied`. The plan is never split between appliers.
#[test]
fn demo_two_appliers_never_split_a_plan() {
    let log = tlog!();
    let demo = behind(
        &log,
        &["wow", "wow_classic"],
        &["retail", "classic"],
        Source::GitHub,
    );
    let home = &demo.home;

    log.step("preview both clients");
    let preview = log.run(home, &["update", "foo", "--dry-run", "--json"]);
    preview.ok();
    let p = plans::plan_id(&preview);
    let approval = preview.data()["approval"].clone();
    log.json("approval", &approval);
    let text = |value: &Value| value.as_str().unwrap_or_default().to_owned();
    let mut changes: Vec<(String, String)> = approval["selection"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|item| (text(&item["target_id"]), text(&item["to_version"])))
        .collect();
    changes.sort();
    let v2 = demo.landing.version(2);
    log.assert_eq(
        "one update per client, to release 2",
        &changes,
        &vec![
            ("classic".to_owned(), v2.clone()),
            ("retail".to_owned(), v2.clone()),
        ],
    );
    let lease = plans::lease_path(&home.env, &p);

    log.step("the first applier commits one client and pauses");
    let pa = Pause::new(home.env.root());
    let a = log
        .cmd(home, &["apply", &p, "--json"])
        .failpoints(&format!("{CLEANUP}=pause"))
        .pause(&pa)
        .spawn();
    log.reached(&pa, CLEANUP);
    let first = plans::history_txs_for(&log, home, &p);
    log.assert_eq("one client committed so far", &first.len(), &1);
    let (x, t1) = first[0].clone();
    let y = if x == "retail" { "classic" } else { "retail" }.to_owned();

    log.step("the second applier waits on the plan's lease");
    let pb = Pause::new(home.env.root());
    let b = log
        .cmd(home, &["apply", &p, "--json"])
        .failpoints(&format!("{PLANNED}=pause,{CLEANUP}=pause"))
        .pause(&pb)
        .spawn();
    plans::wait_until_open(&log, b.pid(), &lease, Duration::from_secs(10));
    log.assert_eq(
        "the second applier holds the lease file open",
        &plans::holds_open(b.pid(), &lease),
        &true,
    );
    // Only a slipped applier's chance to show itself; the checks after both end bind.
    std::thread::sleep(Duration::from_millis(200));
    never_planned(&log, &pb);

    log.step("the first applier finishes");
    log.resume(&pa, CLEANUP);
    let ra = a.wait();
    ra.ok();
    let done = outcomes(&log, &ra);
    let t2 = done
        .iter()
        .find(|(target, _, _)| *target == y)
        .and_then(|(_, _, tx)| tx.clone())
        .unwrap_or_default();
    log.assert_eq(
        "two distinct transactions",
        &(!t2.is_empty() && t2 != t1),
        &true,
    );
    let committed = |target: &str, tx: &str| {
        (
            target.to_owned(),
            "committed".to_owned(),
            Some(tx.to_owned()),
        )
    };
    let mut expected = vec![committed(&x, &t1), committed(&y, &t2)];
    expected.sort();
    log.assert_eq(
        "both clients committed by the first applier",
        &done,
        &expected,
    );

    log.step("the second applier reports the first one's work");
    until_it_ends(&log, &pb, b.pid());
    let rb = b.wait();
    let details = stale(&log, &rb);
    log.assert_eq(
        "already applied, naming both transactions oldest first",
        &(&details["reason"], &details["txs"]),
        &(&json!("already_applied"), &json!([t1, t2])),
    );
    log.assert_eq(
        "it waited for the lease, never for its timeout (elapsed_ms < 30000)",
        &(rb.elapsed_ms < 30_000),
        &true,
    );
    never_planned(&log, &pb);

    log.step("the plan was never split");
    let lines = plans::history_txs_for(&log, home, &p);
    log.assert_eq(
        "exactly two history lines carry the plan",
        &lines,
        &vec![(x.clone(), t1.clone()), (y.clone(), t2.clone())],
    );
    let shown = plans::show(&log, home, &[&p], &[]);
    shown.ok();
    log.assert_eq(
        "plans show: completed, with history's transactions",
        &(&shown.data()["state"], plans::evidence_txs(&shown)),
        &(&json!("completed"), lines.clone()),
    );
    for target in ["retail", "classic"] {
        assert_release(&log, &demo, target, 2);
    }
}

/// An `apply` whose output was lost after it committed is reconciled from history alone:
/// `plans show` reports it completed with one transaction per client, writing nothing and
/// asking nothing, and still names those transactions once the plan store is emptied.
#[test]
fn demo_plans_show_reconciles_a_lost_output() {
    let log = tlog!();
    let demo = behind(
        &log,
        &["wow", "wow_classic"],
        &["retail", "classic"],
        Source::GitHub,
    );
    let home = &demo.home;

    log.step("preview");
    let preview = log.run(home, &["update", "foo", "--dry-run", "--json"]);
    preview.ok();
    let p = plans::plan_id(&preview);
    let approval = preview.data()["approval"].clone();
    log.json("approval", &approval);

    log.step("apply with its output lost");
    let status = plans::apply_with_stdout_closed(&log, home, &p);
    log.note(format!(
        "exit status {status:?}, not asserted: the commits happen before rendering"
    ));

    log.step("reconcile");
    let shown = plans::show(&log, home, &[&p], &[]);
    shown.ok();
    let data = shown.data();
    log.json("plans show", data);
    log.assert_eq(
        "completed, stored, readable, with complete evidence",
        &(
            &data["state"],
            &data["stored"],
            &data["readable"],
            &data["evidence"]["complete"],
        ),
        &(
            &json!("completed"),
            &json!(true),
            &json!(true),
            &json!(true),
        ),
    );
    let evidence = plans::evidence_txs(&shown);
    let mut targets: Vec<&str> = evidence.iter().map(|(target, _)| target.as_str()).collect();
    targets.sort_unstable();
    log.assert_eq(
        "one transaction per client",
        &targets,
        &vec!["classic", "retail"],
    );
    log.assert_eq(
        "the evidence is history's",
        &evidence,
        &plans::history_txs_for(&log, home, &p),
    );
    log.assert_eq(
        "the intent and the preview's approval",
        &(&data["intent"]["command"], &data["approval"]),
        &(&json!("update"), &approval),
    );
    for target in ["retail", "classic"] {
        assert_release(&log, &demo, target, 2);
    }

    log.step("the human answer");
    let human = plans::show_human(&log, home, &[&p], &[]);
    human.exit(0);
    log.assert_eq(
        "stdout says completed",
        &human.stdout().contains("completed"),
        &true,
    );

    log.step("the plan store is emptied");
    let mut removed = Vec::new();
    for path in plans::plan_files(&home.env)
        .into_iter()
        .chain(plans::lease_files(&home.env))
    {
        removed.push(path.file_name().unwrap().to_string_lossy().into_owned());
        fs::remove_file(&path).unwrap();
    }
    log.json("removed", &removed);

    log.step("reconcile without the plan file");
    let gone = plans::show(&log, home, &[&p], &[]);
    gone.ok();
    let data = gone.data();
    log.json("plans show", data);
    log.assert_eq(
        "unknown, not stored, not readable, with no intent or approval",
        &(
            &data["state"],
            &data["stored"],
            &data["readable"],
            &data["intent"],
            &data["approval"],
        ),
        &(
            &json!("unknown"),
            &json!(false),
            &json!(false),
            &Value::Null,
            &Value::Null,
        ),
    );
    log.assert_eq(
        "the same transactions",
        &plans::evidence_txs(&gone),
        &evidence,
    );
}

/// The widget's three Update scenarios of the demo still exist in G1's QML suite under
/// their names, each driving envelopes the binary wrote (no inline envelope) and asserting
/// its outcome. Whether they pass is the widget pipeline's QML step (`argc widget-test`).
#[test]
fn demo_widget_update_qml_scenarios_exist_by_name() {
    let log = tlog!();
    log.step("read the QML suite");
    let qml = fs::read_to_string(repo(PLANS_QML)).unwrap();
    log.note(format!("{PLANS_QML}: {} lines", qml.lines().count()));
    let functions = widget::qml_test_functions(&qml);
    log.json("functions", &functions.keys().collect::<Vec<_>>());
    let wrappers = widget::fixture_wrappers(&qml);
    log.json(
        "helpers that read a named fixture (parameter index)",
        &wrappers,
    );

    log.step("the TestCase");
    log.assert_eq(
        "the TestCase is named Plans (the pipeline's PASS lines say Plans::…)",
        &qml.contains("name: \"Plans\""),
        &true,
    );

    log.step("the three scenarios exist");
    for (item, name, _) in WIDGET_ITEMS {
        log.assert_eq(
            &format!("{item}: {name} is defined"),
            &functions.contains_key(name),
            &true,
        );
    }

    log.step("each drives a real envelope and asserts its outcome");
    for (item, name, asserts) in WIDGET_ITEMS {
        let body = functions.get(name).map(String::as_str).unwrap_or_default();
        let read = widget::fixture_names(body, &wrappers);
        log.json(name, &read);
        log.assert_eq(
            &format!("{item}: reads at least two envelopes (the row's list and a preview)"),
            &(read.len() >= 2),
            &true,
        );
        log.assert_eq(
            &format!("{item}: holds no inline envelope"),
            &(body.contains("\"schema_version\"") || body.contains("\"ok\":")),
            &false,
        );
        log.assert_eq(
            &format!("{item}: asserts on {asserts}"),
            &body.contains(asserts),
            &true,
        );
    }
}

/// The fixtures those scenarios read are ones the binary writes (`PLAN_FIXTURES`, from
/// `plans_fixtures`, which `write_widget_fixtures` calls in pipeline step 1), regenerated
/// as `Fixtures.js` in every pipeline run inside the directory it removes, and never
/// committed: a QML run can read only what that run's binary wrote.
#[test]
fn demo_widget_update_qml_scenarios_read_only_fixtures_the_binary_writes() {
    let log = tlog!();
    log.step("the fixtures the scenarios read");
    let qml = fs::read_to_string(repo(PLANS_QML)).unwrap();
    let functions = widget::qml_test_functions(&qml);
    let wrappers = widget::fixture_wrappers(&qml);
    let mut read: Vec<String> = Vec::new();
    for (_, name, _) in WIDGET_ITEMS {
        let body = functions.get(name).map(String::as_str).unwrap_or_default();
        for fixture in widget::fixture_names(body, &wrappers) {
            if !read.contains(&fixture) {
                read.push(fixture);
            }
        }
    }
    log.json("fixtures read", &read);
    log.assert_eq("the scenarios read fixtures", &read.is_empty(), &false);

    log.step("each is one the binary writes");
    let source = fs::read_to_string(repo("tests/omarchy_widget.rs")).unwrap();
    let list = widget::plan_fixture_list(&source);
    log.json("PLAN_FIXTURES", &list);
    log.assert_eq("PLAN_FIXTURES lists fixtures", &list.is_empty(), &false);
    for fixture in &read {
        log.assert_eq(
            &format!("{fixture} is in PLAN_FIXTURES"),
            &list.contains(fixture),
            &true,
        );
    }
    let code = widget::code_only(&source);
    log.assert_eq(
        "tests/omarchy_widget.rs defines plans_fixtures",
        &code.contains("fn plans_fixtures("),
        &true,
    );
    let writer = widget::braced_body(&source, "fn write_widget_fixtures(").unwrap_or_default();
    log.note(format!(
        "write_widget_fixtures: {} lines",
        writer.lines().count()
    ));
    log.assert_eq(
        "write_widget_fixtures calls plans_fixtures",
        &widget::code_only(writer).contains("plans_fixtures("),
        &true,
    );

    log.step("they are generated in the same pipeline run");
    let script = fs::read_to_string(repo("scripts/widget-test.sh")).unwrap();
    let order = widget::in_order(
        &script,
        &[
            "--exact omarchy_widget::write_widget_fixtures",
            "cp -r omarchy \"$plugin\"",
            "omarchy/tests/qml_fixtures.js",
            "\"$runner\" -input",
        ],
    );
    log.json(
        "offsets: step 1, the plugin copy, the generator, the runner",
        &order,
    );
    log.assert_eq(
        "step 1, then the plugin copy, then the generator, then the runner",
        &order.err(),
        &None,
    );
    let generator = script
        .lines()
        .find(|line| line.contains("omarchy/tests/qml_fixtures.js"))
        .unwrap_or_default();
    log.note(format!("the generator: {}", generator.trim()));
    log.assert_eq(
        "it reads step 1's fixtures and writes the plugin copy's Fixtures.js",
        &(
            generator.contains("\"$OMADDONS_WIDGET_OUT/fixtures\""),
            generator.contains("\"$plugin/tests/qml/Fixtures.js\""),
        ),
        &(true, true),
    );
    log.assert_eq(
        "the plugin copy is inside the run's directory, which the trap removes",
        &(
            script
                .lines()
                .any(|line| line.trim() == "plugin=$OMADDONS_WIDGET_OUT/plugin"),
            script.contains("trap 'rm -rf \"$OMADDONS_WIDGET_OUT\"' EXIT"),
        ),
        &(true, true),
    );

    log.step("nothing generated is committed");
    let mut files = Vec::new();
    regular_files(&repo("omarchy"), &mut files);
    log.note(format!("walked {} files under omarchy/", files.len()));
    let generated: Vec<String> = files
        .iter()
        .filter(|path| {
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            name == "Fixtures.js" || read.iter().any(|fixture| name == format!("{fixture}.json"))
        })
        .map(|path| path.display().to_string())
        .collect();
    log.assert_eq(
        "no Fixtures.js, and no copy of a fixture the scenarios read, under omarchy/",
        &generated,
        &Vec::<String>::new(),
    );
}
