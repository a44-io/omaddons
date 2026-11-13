//! The determinism scan (M4-D18): no HashMap or HashSet in a type that derives Serialize in the output directories,
//! because a hash map's iteration order differs per process. The run-time half is the contract suite's repeated runs
//! (`tests/contract/special.rs`), which catch one only where an output holds two or more members.

// The crate's production-source view, the same file the library's unit-test scans use: one lexer, not a copy.
#[allow(dead_code)]
#[path = "../src/source_scan.rs"]
mod source_scan;

use std::path::Path;

use source_scan::{Literals, matching, production_code, sources};

/// The directories M4-D18 names, and `src/errors`, whose errors ledger is a persisted file whose bytes decide
/// whether a store is needed (RG-10a).
const SCANNED: &[&str] = &[
    "src/app",
    "src/output",
    "src/contract",
    "src/doctor",
    "src/errors",
];

/// One finding: the line (1-based), the item's name and the line's text (as the blanked view shows it).
#[derive(Debug, PartialEq, Eq)]
struct Hit {
    line: usize,
    item: String,
    text: String,
}

/// One `struct` or `enum` whose derive list names `Serialize`: its name and its extent in chars, from its name
/// to just past its end (generics and `where` clauses included).
#[derive(Debug)]
struct Serialized {
    name: String,
    start: usize,
    end: usize,
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The identifier starting at `i` (empty when none does).
fn word_at(code: &[char], i: usize) -> String {
    code.get(i..)
        .unwrap_or_default()
        .iter()
        .take_while(|c| is_ident(**c))
        .collect()
}

fn skip_ws(code: &[char], mut i: usize) -> usize {
    while code.get(i).is_some_and(|c| c.is_whitespace()) {
        i += 1;
    }
    i
}

/// Whether an attribute's text (inside `#[…]`) is a derive list naming `Serialize` or `<path>::Serialize`.
fn derives_serialize(attr: &[char]) -> bool {
    let attr: String = attr.iter().filter(|c| !c.is_whitespace()).collect();
    attr.strip_prefix("derive(")
        .and_then(|rest| rest.strip_suffix(')'))
        .is_some_and(|list| {
            list.split(',')
                .any(|entry| entry == "Serialize" || entry.ends_with("::Serialize"))
        })
}

/// Just past the end of the item whose name ends at `from`: the `}` closing its first `{` at depth zero, or a
/// tuple struct's `;` after its `( … )` (and any `where` clause), or a unit struct's `;`. Brackets inside its
/// generics or `where` clause (`fn(u8)`, `Fn(u8) -> u8`) are skipped whole.
fn item_end(code: &[char], from: usize) -> Option<usize> {
    let (mut angle, mut after_where, mut tuple_body) = (0usize, false, false);
    let mut k = from;
    while let Some(&c) = code.get(k) {
        match c {
            '<' => angle += 1,
            // The `>` of a `->`.
            '>' if k > 0 && code[k - 1] == '-' => {}
            '>' => angle = angle.saturating_sub(1),
            '{' if angle == 0 && !tuple_body => return matching(code, k).map(|close| close + 1),
            '(' if angle == 0 && !after_where && !tuple_body => {
                k = matching(code, k)?;
                tuple_body = true;
            }
            '(' | '[' | '{' => k = matching(code, k)?,
            ';' if angle == 0 => return Some(k + 1),
            c if is_ident(c) => {
                let word = word_at(code, k);
                after_where |= angle == 0 && word == "where";
                k += word.len();
                continue;
            }
            _ => {}
        }
        k += 1;
    }
    None
}

/// The item after a derive attribute that ends at `from`: further attributes, then an optional visibility
/// (`pub`, `pub(crate)`, `pub(in <path>)`, …), then `struct` or `enum` and its name. `None` for anything else.
fn serialized_item(code: &[char], from: usize) -> Option<Serialized> {
    let mut j = skip_ws(code, from);
    while code.get(j) == Some(&'#') && code.get(j + 1) == Some(&'[') {
        j = skip_ws(code, matching(code, j + 1)? + 1);
    }
    if word_at(code, j) == "pub" {
        j = skip_ws(code, j + "pub".len());
        if code.get(j) == Some(&'(') {
            j = skip_ws(code, matching(code, j)? + 1);
        }
    }
    let keyword = word_at(code, j);
    if keyword != "struct" && keyword != "enum" {
        return None;
    }
    let start = skip_ws(code, j + keyword.len());
    let name = word_at(code, start);
    if name.is_empty() {
        return None;
    }
    let end = item_end(code, start + name.len())?;
    Some(Serialized { name, start, end })
}

/// Every `struct` and `enum` deriving `Serialize` in `code`, which is already
/// `production_code(…, Literals::Blank)`.
fn serialized_items(code: &[char]) -> Vec<Serialized> {
    let mut items = Vec::new();
    let mut i = 0;
    while i < code.len() {
        if code[i] != '#' || code.get(i + 1) != Some(&'[') {
            i += 1;
            continue;
        }
        let Some(close) = matching(code, i + 1) else {
            break;
        };
        if derives_serialize(&code[i + 2..close])
            && let Some(item) = serialized_item(code, close + 1)
        {
            items.push(item);
        }
        i = close + 1;
    }
    items
}

/// Each `HashMap<` or `HashSet<` within the extent of a `struct` or `enum` whose derive list contains `Serialize`,
/// in `code`, which is already `production_code(…, Literals::Blank)`.
fn serialized_hash_fields(code: &str) -> Vec<Hit> {
    let chars: Vec<char> = code.chars().collect();
    let lines: Vec<&str> = code.lines().collect();
    let mut hits = Vec::new();
    for item in serialized_items(&chars) {
        let mut k = item.start;
        while k < item.end {
            if !is_ident(chars[k]) {
                k += 1;
                continue;
            }
            // Whole words only, so `MyHashMap` is no hit and `std::collections::HashMap` is.
            let word = word_at(&chars, k);
            if matches!(word.as_str(), "HashMap" | "HashSet")
                && chars.get(skip_ws(&chars, k + word.len())) == Some(&'<')
            {
                let line = chars[..k].iter().filter(|c| **c == '\n').count() + 1;
                hits.push(Hit {
                    line,
                    item: item.name.clone(),
                    text: lines.get(line - 1).map_or("", |l| l.trim()).to_owned(),
                });
            }
            k += word.len();
        }
    }
    hits
}

fn repo() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn no_serialized_type_holds_a_hash_map_or_set() {
    let mut found = Vec::new();
    for dir in SCANNED {
        for (path, text) in sources(&repo().join(dir)) {
            let Some(code) = production_code(&text, Literals::Blank) else {
                continue;
            };
            for hit in serialized_hash_fields(&code) {
                let rel = path.strip_prefix(repo()).unwrap_or(&path).display();
                let source = text
                    .lines()
                    .nth(hit.line - 1)
                    .map_or(hit.text.as_str(), str::trim);
                found.push(format!("{rel}:{} {}: {source}", hit.line, hit.item));
            }
        }
    }
    assert!(
        found.is_empty(),
        "{}\nuse BTreeMap or BTreeSet (DEC-3, M4-D18): a hash map's iteration order differs per process, so the \
         output would differ between runs",
        found.join("\n")
    );
}

/// A rename or move must not silently empty the scan: if a directory is renamed, update `SCANNED` with it.
#[test]
fn the_scanned_directories_exist() {
    for dir in SCANNED {
        assert!(
            repo().join(dir).is_dir(),
            "{dir} is not a directory: update SCANNED in tests/determinism.rs with its new place"
        );
    }
}

/// The scan reads the real tree's derive lists: a scanner that matched no item would pass vacuously.
#[test]
fn the_scan_sees_the_serialized_types() {
    let mut names = Vec::new();
    for dir in SCANNED {
        for (_, text) in sources(&repo().join(dir)) {
            if let Some(code) = production_code(&text, Literals::Blank) {
                let chars: Vec<char> = code.chars().collect();
                names.extend(serialized_items(&chars).into_iter().map(|item| item.name));
            }
        }
    }
    for expected in ["Envelope", "StatusData", "Finding"] {
        assert!(
            names.iter().any(|name| name == expected),
            "{expected} derives Serialize but the scan did not see it; seen {} items",
            names.len()
        );
    }
}

/// A planted case: its name, its input and the hits expected, as `(item, line)`.
type Case = (&'static str, &'static str, &'static [(&'static str, usize)]);

#[test]
fn the_scan_finds_what_it_must() {
    let cases: &[Case] = &[
        (
            "hash_map_field",
            "#[derive(Serialize)] struct A { m: HashMap<String, u8> }",
            &[("A", 1)],
        ),
        (
            "qualified_hash_set",
            "#[derive(Debug, Clone, Serialize, JsonSchema)] pub struct A { pub s: std::collections::HashSet<u8> }",
            &[("A", 1)],
        ),
        (
            "enum_variant_field",
            "#[derive(Serialize)] pub enum E { V { m: HashMap<u8, u8> }, W }",
            &[("E", 1)],
        ),
        (
            "tuple_struct",
            "#[derive(Serialize)] pub struct T(pub HashMap<u8, u8>);",
            &[("T", 1)],
        ),
        (
            "serde_attr_between",
            "#[derive(Serialize)]\n#[serde(rename_all = \"snake_case\")]\npub(crate) struct A { m: Option<HashMap<u8, u8>> }",
            &[("A", 3)],
        ),
        (
            "generic_default",
            "#[derive(Serialize)] struct A<M = HashMap<u8, u8>> { m: M }",
            &[("A", 1)],
        ),
        (
            "serde_path_derive",
            "#[derive(Debug, serde::Serialize)] struct A { m: HashMap<u8, u8> }",
            &[("A", 1)],
        ),
        (
            "two_hits_one_item",
            "#[derive(Serialize)]\nstruct A {\n    m: HashMap<u8, u8>,\n    s: HashSet<u8>,\n}",
            &[("A", 3), ("A", 4)],
        ),
        (
            "btree_map",
            "#[derive(Serialize)] struct A { m: BTreeMap<u8, u8> }",
            &[],
        ),
        (
            "debug_only_derive",
            "#[derive(Debug)] struct A { m: HashMap<u8, u8> }",
            &[],
        ),
        (
            "deserialize_only",
            "#[derive(Deserialize)] struct A { m: HashMap<u8, u8> }",
            &[],
        ),
        (
            "in_a_comment",
            "// #[derive(Serialize)] struct A { m: HashMap<u8, u8> }",
            &[],
        ),
        (
            "in_a_string",
            "#[derive(Serialize)] struct A { s: &'static str }\nconst X: &str = \"HashMap<u8>\";",
            &[],
        ),
        (
            "test_only_item",
            "#[cfg(test)]\n#[derive(Serialize)] struct A { m: HashMap<u8, u8> }",
            &[],
        ),
        (
            "next_item_not_serialized",
            "#[derive(Serialize)] struct A { n: u8 }\nstruct B { m: HashMap<u8, u8> }",
            &[],
        ),
        (
            "unit_struct_then_other",
            "#[derive(Serialize)] struct U;\nstruct B { m: HashMap<u8, u8> }",
            &[],
        ),
        (
            "similar_name",
            "#[derive(Serialize)] struct A { m: MyHashMap<u8> }",
            &[],
        ),
        (
            "pub_in_path",
            "#[derive(Serialize)] pub(in crate::app) struct A { m: HashSet<u8> }",
            &[("A", 1)],
        ),
        (
            "where_clause_brackets",
            "#[derive(Serialize)] struct A<F> where F: Fn(u8) -> u8 { f: F, m: HashMap<u8, u8> }\nstruct B { m: HashSet<u8> }",
            &[("A", 1)],
        ),
        (
            "tuple_struct_where_clause",
            "#[derive(Serialize)] struct T<A>(A, HashSet<u8>) where A: Fn(u8);\nstruct B { m: HashMap<u8, u8> }",
            &[("T", 1)],
        ),
        (
            "multi_line_derive",
            "#[derive(\n    Debug,\n    Serialize,\n)]\nstruct A {\n    m: HashMap<u8, u8>,\n}",
            &[("A", 6)],
        ),
    ];
    for (case, input, expected) in cases {
        let code = production_code(input, Literals::Blank).unwrap();
        let hits = serialized_hash_fields(&code);
        let found: Vec<(&str, usize)> = hits
            .iter()
            .map(|hit| (hit.item.as_str(), hit.line))
            .collect();
        assert_eq!(
            &found, expected,
            "case {case}: input {input:?}, hits {hits:?}"
        );
    }
}
