//! The production view of this crate's own source, for the unit tests that scan it:
//! comments dropped and every test-only item (`#[cfg(test)]`, `#[cfg(all(test, …))]`)
//! blanked wherever it sits, so code after an early `#[cfg(test)] mod tests;` is still
//! seen. Splitting a file at its first `#[cfg(test)]` instead misses that code, and
//! stops early at a `#[cfg(test)]` written in a string. The lexer and the item rules are
//! `tests/single_writer.rs`'s, which cannot share them (an integration test sees no
//! `cfg(test)` code).

// The single-writer scan skips files marked test-only.
#![cfg(test)]

use std::path::{Path, PathBuf};

/// What a scan sees of string and char literals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Literals {
    /// Their contents, for a scan that reads them (`failpoint::hit("engine.…")`).
    Keep,
    /// Spaces, for a scan of code only.
    Blank,
}

/// `text`'s production code, char for char: comments, test-only items and (with
/// [`Literals::Blank`]) literal contents become spaces, newlines kept. `None` for a file
/// marked `#![cfg(test)]`.
pub(crate) fn production_code(text: &str, literals: Literals) -> Option<String> {
    let code = mask(text, false);
    let joined: String = code.iter().collect();
    if joined.lines().any(|line| line.trim() == "#![cfg(test)]") {
        return None;
    }
    let mut out = match literals {
        Literals::Keep => mask(text, true),
        Literals::Blank => code.clone(),
    };
    // Test items are found on the blanked view, where no literal can hold a bracket.
    let mut i = 0;
    while i < code.len() {
        if code[i] == '#' && code.get(i + 1) == Some(&'[') {
            let Some(close) = matching(&code, i + 1) else {
                break;
            };
            let attr: String = code[i + 2..close].iter().collect();
            if requires_test(&attr) {
                let end = item_end(&code, close + 1);
                for c in &mut out[i..end] {
                    if *c != '\n' {
                        *c = ' ';
                    }
                }
                i = end;
            } else {
                i = close + 1;
            }
            continue;
        }
        i += 1;
    }
    Some(out.into_iter().collect())
}

/// Every `.rs` file under `dir`, recursively, with its text, sorted by path.
pub(crate) fn sources(dir: &Path) -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(next) = pending.pop() {
        for entry in std::fs::read_dir(&next).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let text = std::fs::read_to_string(&path).unwrap();
                out.push((path, text));
            }
        }
    }
    out.sort();
    out
}

/// Words that start an item: a top-level `,` ends no item that starts with one (the
/// commas of `impl<A, B>` or `fn f<A, B>`), but ends a variant, a field or an arm.
const ITEM_WORDS: &[&str] = &[
    "pub",
    "fn",
    "mod",
    "use",
    "impl",
    "struct",
    "enum",
    "union",
    "trait",
    "type",
    "const",
    "static",
    "extern",
    "unsafe",
    "async",
    "macro_rules",
    "let",
];

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

enum State {
    Code,
    Str,
    /// A raw string with this many `#`.
    Raw(usize),
    /// A block comment this deeply nested.
    Block(usize),
}

/// `text` char for char, comments as spaces and, unless `keep_literals`, the contents of
/// string, raw string and char literals too (their quotes kept); newlines stay.
fn mask(text: &str, keep_literals: bool) -> Vec<char> {
    let chars: Vec<char> = text.chars().collect();
    let at = |i: usize| chars.get(i).copied();
    let mut out = chars.clone();
    let blank = |out: &mut Vec<char>, i: usize| {
        if let Some(c) = out.get_mut(i)
            && *c != '\n'
        {
            *c = ' ';
        }
    };
    let mut state = State::Code;
    let mut i = 0;
    while let Some(c) = at(i) {
        match state {
            State::Code => {
                if c == '/' && at(i + 1) == Some('/') {
                    while at(i).is_some_and(|c| c != '\n') {
                        blank(&mut out, i);
                        i += 1;
                    }
                    continue;
                }
                if c == '/' && at(i + 1) == Some('*') {
                    blank(&mut out, i);
                    blank(&mut out, i + 1);
                    state = State::Block(1);
                    i += 2;
                    continue;
                }
                if c == '"' {
                    state = State::Str;
                    i += 1;
                    continue;
                }
                // `r"`, `r#"`, `br"`, `cr"`: not the `r` of a longer identifier.
                let word: String = chars[..i]
                    .iter()
                    .rev()
                    .take_while(|c| is_ident(**c))
                    .collect();
                if c == 'r' && matches!(word.as_str(), "" | "b" | "c") {
                    let hashes = (i + 1..).take_while(|&j| at(j) == Some('#')).count();
                    if at(i + 1 + hashes) == Some('"') {
                        state = State::Raw(hashes);
                        i += 2 + hashes;
                        continue;
                    }
                }
                if c == '\''
                    && let Some(end) = char_literal_end(&chars, i)
                {
                    if !keep_literals {
                        for j in i + 1..end {
                            blank(&mut out, j);
                        }
                    }
                    i = end + 1;
                    continue;
                }
                i += 1;
            }
            State::Str => match c {
                '\\' => {
                    if !keep_literals {
                        blank(&mut out, i);
                        blank(&mut out, i + 1);
                    }
                    i += 2;
                }
                '"' => {
                    state = State::Code;
                    i += 1;
                }
                _ => {
                    if !keep_literals {
                        blank(&mut out, i);
                    }
                    i += 1;
                }
            },
            State::Raw(hashes) => {
                if c == '"' && (1..=hashes).all(|k| at(i + k) == Some('#')) {
                    state = State::Code;
                    i += 1 + hashes;
                } else {
                    if !keep_literals {
                        blank(&mut out, i);
                    }
                    i += 1;
                }
            }
            State::Block(depth) => {
                blank(&mut out, i);
                if c == '/' && at(i + 1) == Some('*') {
                    blank(&mut out, i + 1);
                    state = State::Block(depth + 1);
                    i += 2;
                } else if c == '*' && at(i + 1) == Some('/') {
                    blank(&mut out, i + 1);
                    state = if depth == 1 {
                        State::Code
                    } else {
                        State::Block(depth - 1)
                    };
                    i += 2;
                } else {
                    i += 1;
                }
            }
        }
    }
    out
}

/// The index of the closing quote when `chars[i]` opens a char literal (`'x'`, `'\n'`,
/// `'\u{1F600}'`); `None` for a lifetime or a label (`'a`).
fn char_literal_end(chars: &[char], i: usize) -> Option<usize> {
    match chars.get(i + 1)? {
        '\\' => (i + 3..chars.len())
            .take_while(|&j| chars[j] != '\n')
            .find(|&j| chars[j] == '\''),
        '\n' => None,
        _ => (chars.get(i + 2) == Some(&'\'')).then_some(i + 2),
    }
}

/// The index of the bracket closing the one at `open` (any of `()[]{}`).
fn matching(code: &[char], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (j, c) in code.iter().enumerate().skip(open) {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(j);
                }
            }
            _ => {}
        }
    }
    None
}

/// The index just past the item starting at `from` (after its `cfg`): its further
/// attributes, then up to a `;` outside brackets or through its first `{ … }`; a `,`
/// ends it too unless it starts with an [`ITEM_WORDS`] word. It never runs past the
/// bracket that encloses it.
fn item_end(code: &[char], from: usize) -> usize {
    let mut i = from;
    let mut first_word: Option<String> = None;
    while i < code.len() {
        let c = code[i];
        if first_word.is_none() && is_ident(c) {
            first_word = Some(code[i..].iter().take_while(|c| is_ident(**c)).collect());
        }
        match c {
            ';' => return i + 1,
            ',' if !first_word
                .as_deref()
                .is_some_and(|w| ITEM_WORDS.contains(&w)) =>
            {
                return i + 1;
            }
            '{' => return matching(code, i).map_or(code.len(), |close| close + 1),
            // An attribute (`#[…]`), arguments, an array type.
            '(' | '[' => i = matching(code, i).map_or(code.len(), |close| close + 1),
            ')' | ']' | '}' => return i,
            _ => i += 1,
        }
    }
    code.len()
}

/// Whether an attribute's text (inside `#[…]`) is a `cfg` that holds only in tests:
/// `test`, or an `all(…)` with such a predicate. `not(test)` and `any(test, …)` do not.
fn requires_test(attr: &str) -> bool {
    fn holds_only_in_tests(predicate: &str) -> bool {
        predicate == "test"
            || predicate
                .strip_prefix("all(")
                .and_then(|rest| rest.strip_suffix(')'))
                .is_some_and(|args| top_level(args).into_iter().any(holds_only_in_tests))
    }
    let attr: String = attr.chars().filter(|c| !c.is_whitespace()).collect();
    attr.strip_prefix("cfg(")
        .and_then(|rest| rest.strip_suffix(')'))
        .is_some_and(holds_only_in_tests)
}

/// `args` split at the commas outside parentheses.
fn top_level(args: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let (mut depth, mut start) = (0usize, 0);
    for (j, c) in args.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                parts.push(&args[start..j]);
                start = j + 1;
            }
            _ => {}
        }
    }
    parts.push(&args[start..]);
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file whose `mod tests;` comes first, as `src/net/mod.rs`'s does: the code after
    /// it is seen, comments and test-only items are not, and a `#[cfg(test)]` written in
    /// a string ends nothing.
    const PROBE: &str = r##"
#[cfg(test)]
mod tests;

/// A failpoint in a comment: failpoint::hit("engine.commented")
pub fn late() -> &'static str {
    let _ = failpoint::hit("engine.late"); /* lock_file in a comment */
    let _ = 'x';
    let _ = r#"raw "quoted" lock_file"#;
    "#[cfg(test)] lock_file in a string"
}

impl Versioned for Late {}

#[cfg(all(test, debug_assertions))]
fn test_only() {
    failpoint::hit("engine.test_only");
}

pub struct After;
"##;

    #[test]
    fn code_after_an_early_mod_tests_is_production() {
        let keep = production_code(PROBE, Literals::Keep).unwrap();
        assert!(keep.contains("failpoint::hit(\"engine.late\")"), "{keep}");
        assert!(!keep.contains("engine.commented"));
        assert!(!keep.contains("engine.test_only"));
        assert!(!keep.contains("mod tests;"));
        assert!(keep.contains("impl Versioned for Late"));
        assert!(
            keep.contains("pub struct After"),
            "past the string's #[cfg(test)]"
        );

        let blank = production_code(PROBE, Literals::Blank).unwrap();
        assert!(
            !blank.contains("lock_file"),
            "in comments and literals only"
        );
        assert!(blank.contains("failpoint::hit(\""));
        assert!(blank.contains("pub struct After"));
        // Char for char: every line keeps its number.
        assert_eq!(blank.lines().count(), PROBE.lines().count());
        assert_eq!(keep.chars().count(), PROBE.chars().count());
    }

    #[test]
    fn a_test_only_file_has_no_production_code() {
        assert_eq!(
            production_code("//! x\n#![cfg(test)]\nfn f() {}\n", Literals::Blank),
            None
        );
    }

    /// The crate's own `src/net/mod.rs` starts with `#[cfg(test)] mod tests;`: its
    /// production code is still seen.
    #[test]
    fn src_net_mod_rs_is_scanned_past_its_mod_tests() {
        let text = include_str!("net/mod.rs");
        let first = text.find("#[cfg(test)]").unwrap();
        let code = production_code(text, Literals::Blank).unwrap();
        assert!(code[first..].contains("pub struct Request"));
    }
}
