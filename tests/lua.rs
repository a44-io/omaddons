//! The SavedVariables parser (`src/lua.rs`) against the shared writer renderer: any tree
//! the client could write reads back equal in both writer formats, any bytes parse
//! within the bounds, and an oversized file is refused before it is read.

mod common;

use std::fs;

use common::lua::{self as writer, Field, Writer};
use omaddons::limits::{LUA_MAX_ISSUES, LUA_MAX_NODES, SAVEDVARIABLES_MAX_BYTES};
use omaddons::lua::{
    Key, Parsed, Table, Value, is_too_large, parse_saved_variables, parse_with_stats, read_bounded,
    step_bound,
};
use proptest::prelude::*;

/// What the parser reads for a value the renderer wrote: positional fields to `array`,
/// keyed ones to `hash`, each in order.
fn expected(value: &writer::Value) -> Value {
    match value {
        writer::Value::Bool(b) => Value::Bool(*b),
        writer::Value::Int(n) => Value::Int(*n),
        writer::Value::Num(x) => Value::Num(*x),
        writer::Value::Str(bytes) => Value::Str(bytes.clone()),
        writer::Value::Table(t) => {
            let mut table = Table::default();
            for field in &t.0 {
                match field {
                    Field::Pos(v) => table.array.push(expected(v)),
                    Field::Key(k, v) => {
                        let key = match k {
                            writer::Key::Str(s) => Key::Str(s.as_bytes().to_vec()),
                            writer::Key::Int(n) => Key::Int(*n),
                        };
                        table.hash.push((key, expected(v)));
                    }
                }
            }
            Value::Table(table)
        }
    }
}

fn leaf() -> impl Strategy<Value = writer::Value> {
    prop_oneof![
        any::<bool>().prop_map(writer::Value::Bool),
        any::<i64>().prop_map(writer::Value::Int),
        // Finite and infinite floats; NaN never equals itself.
        any::<f64>()
            .prop_filter("not NaN", |x| !x.is_nan())
            .prop_map(writer::Value::Num),
        proptest::collection::vec(any::<u8>(), 0..24).prop_map(writer::Value::Str),
    ]
}

fn tree() -> impl Strategy<Value = writer::Value> {
    leaf().prop_recursive(6, 64, 8, |inner| {
        proptest::collection::vec(
            prop_oneof![
                inner.clone().prop_map(Field::Pos),
                ("[a-zA-Z_][a-zA-Z0-9_ ]{0,8}", inner.clone())
                    .prop_map(|(k, v)| Field::Key(writer::Key::Str(k), v)),
                (any::<i64>(), inner).prop_map(|(k, v)| Field::Key(writer::Key::Int(k), v)),
            ],
            0..8,
        )
        .prop_map(|fields| writer::Value::Table(writer::Table(fields)))
    })
}

fn names() -> impl Strategy<Value = Vec<String>> {
    proptest::collection::btree_set("[A-Za-z_][A-Za-z0-9_]{0,10}", 1..4)
        .prop_map(|set| set.into_iter().collect())
}

/// The bounds every parse keeps, whatever the input.
fn assert_bounded(bytes: &[u8], wanted: &[&str]) -> Parsed {
    let (parsed, stats) = parse_with_stats(bytes, wanted);
    assert!(stats.nodes <= LUA_MAX_NODES);
    assert!(parsed.issues.len() <= LUA_MAX_ISSUES);
    assert!(stats.steps <= step_bound(bytes.len()), "{stats:?}");
    if wanted.is_empty() {
        assert_eq!((stats.nodes, parsed.globals.len()), (0, 0));
    }
    parsed
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Any tree, rendered as either client writes it, reads back equal, and the two
    /// formats read the same.
    #[test]
    fn rendered_values_parse_back(names in names(), trees in proptest::collection::vec(tree(), 4)) {
        let globals: Vec<(&str, writer::Value)> = names
            .iter()
            .map(String::as_str)
            .zip(trees)
            .collect();
        let wanted: Vec<&str> = names.iter().map(String::as_str).collect();
        let mut reads = Vec::new();
        for format in Writer::ALL {
            let bytes = writer::render(format, &globals);
            let parsed = assert_bounded(&bytes, &wanted);
            prop_assert!(parsed.issues.is_empty() && parsed.complete, "{format:?}: {parsed:?}");
            for (name, value) in &globals {
                prop_assert_eq!(parsed.globals.get(*name), Some(&expected(value)));
            }
            reads.push(parsed);
        }
        prop_assert_eq!(&reads[0], &reads[1]);
    }

    /// Arbitrary bytes, and arbitrary cuts and splices of a real rendering, never panic
    /// and stay within every bound; the same bytes always parse the same.
    #[test]
    fn arbitrary_bytes_never_panic(
        bytes in proptest::collection::vec(any::<u8>(), 0..512),
        tree in tree(),
        cut in any::<prop::sample::Index>(),
        wanted in prop_oneof![Just(vec![]), Just(vec!["T"]), Just(vec!["T", "X"])],
    ) {
        let rendered = writer::render(Writer::Legacy, &[("T", tree)]);
        let at = cut.index(rendered.len() + 1);
        let mut spliced = rendered[..at].to_vec();
        spliced.extend_from_slice(&bytes);
        for input in [&bytes[..], &rendered[..at], &spliced[..]] {
            let first = assert_bounded(input, &wanted);
            let again = parse_saved_variables(input, &wanted);
            // NaN never equals itself, so compare what was read as text.
            prop_assert_eq!(format!("{first:?}"), format!("{again:?}"));
        }
    }
}

#[test]
fn a_file_over_the_cap_is_refused_unread() {
    let dir = tempfile::tempdir().unwrap();
    let big = dir.path().join("big.lua");
    // Sparse: the cap is checked before any byte is read.
    fs::File::create(&big)
        .unwrap()
        .set_len(SAVEDVARIABLES_MAX_BYTES + 1)
        .unwrap();
    let err = read_bounded(&big).unwrap_err();
    assert!(is_too_large(&err), "{err}");
    assert_eq!(err.kind(), std::io::ErrorKind::FileTooLarge);

    let small = dir.path().join("small.lua");
    fs::write(&small, b"\r\nT = {\r\n1,\r\n}\r\n").unwrap();
    let bytes = read_bounded(&small).unwrap();
    let parsed = parse_saved_variables(&bytes, &["T"]);
    assert!(parsed.complete && parsed.issues.is_empty());

    let missing = read_bounded(&dir.path().join("missing.lua")).unwrap_err();
    assert!(!is_too_large(&missing));
    let at_cap = dir.path().join("at_cap.lua");
    fs::File::create(&at_cap)
        .unwrap()
        .set_len(SAVEDVARIABLES_MAX_BYTES)
        .unwrap();
    assert_eq!(
        read_bounded(&at_cap).unwrap().len() as u64,
        SAVEDVARIABLES_MAX_BYTES,
        "the cap itself is allowed"
    );
}
