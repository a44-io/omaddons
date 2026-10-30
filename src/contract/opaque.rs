//! The schema positions that admit any value on purpose, each with the Rust field it comes
//! from and why. The contract suite's strict form leaves them open (they are structurally
//! open: [`super::schema::is_open`]), and the semantic manifest records them as `opaque`.

/// A schema position that admits any value on purpose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Opaque {
    /// `envelope`, `error`, `event <name>`, `command <path>`, `command *` (every command)
    /// or `file <kind>`.
    pub scope: &'static str,
    /// A [`super::schema::walk`] path; a leading `**.` matches at any depth.
    pub path: &'static str,
    /// The Rust type and field it comes from, `Setting.value`.
    pub source: &'static str,
    /// One sentence: why it is unconstrained.
    pub reason: &'static str,
}

impl Opaque {
    /// Whether this row covers the position at `path` of the schema published in `scope`.
    pub fn matches(&self, scope: &str, path: &str) -> bool {
        let scope_matches =
            self.scope == scope || (self.scope == "command *" && scope.starts_with("command "));
        let path_matches = match self.path.strip_prefix("**.") {
            Some(suffix) => {
                path == suffix
                    || path
                        .strip_suffix(suffix)
                        .is_some_and(|head| head.ends_with('.'))
            }
            None => path == self.path,
        };
        scope_matches && path_matches
    }
}

const ITEM_ERROR_DETAILS: &str =
    "An item error keeps the subset of its own code's details that the item needs.";
const CONFIG_VALUE: &str = "A config value is whatever TOML value its key admits, and each key's type is published with the config keys.";
const LEFT_OUT_FOLDER: &str = "A left-out folder carries its reason's own fields, kept as written so a reason a newer build wrote still reads.";
const PRECONDITION_PART: &str = "A binding token's part is kept as the canonical value its token digests, and loading the plan recomputes the token from it.";

/// Every open position of every published schema, sorted by scope, then path.
#[rustfmt::skip]
pub const OPAQUE: &[Opaque] = &[
    Opaque { scope: "command *", path: "**.items[].error.details", source: "ItemError.details", reason: ITEM_ERROR_DETAILS },
    Opaque { scope: "command apply", path: "result.targets[].error.details", source: "ItemError.details", reason: ITEM_ERROR_DETAILS },
    Opaque { scope: "command apply", path: "result.transactions[].error.details", source: "ItemError.details", reason: ITEM_ERROR_DETAILS },
    Opaque { scope: "command backup create", path: "targets[].error.details", source: "ItemError.details", reason: ITEM_ERROR_DETAILS },
    Opaque { scope: "command backup prune", path: "targets[].error.details", source: "ItemError.details", reason: ITEM_ERROR_DETAILS },
    Opaque { scope: "command config", path: "settings[].value", source: "Setting.value", reason: CONFIG_VALUE },
    Opaque { scope: "command config get", path: "default", source: "GetData.default", reason: CONFIG_VALUE },
    Opaque { scope: "command config get", path: "value", source: "GetData.value", reason: CONFIG_VALUE },
    Opaque { scope: "command config set", path: "previous", source: "SetData.previous", reason: CONFIG_VALUE },
    Opaque { scope: "command config set", path: "value", source: "SetData.value", reason: CONFIG_VALUE },
    Opaque { scope: "command info", path: "**.left_out[]", source: "LeftOutFolder.details", reason: LEFT_OUT_FOLDER },
    Opaque { scope: "command list", path: "**.left_out[]", source: "LeftOutFolder.details", reason: LEFT_OUT_FOLDER },
    Opaque { scope: "command robot-docs errors", path: "error_codes[].details_schema", source: "ErrorCodeDoc.details_schema", reason: "A code's `details` schema is a JSON Schema fragment, written in JSON Schema's own vocabulary." },
    Opaque { scope: "command robot-docs schema", path: "commands{}", source: "SchemaData.commands", reason: "Each command's published data schema is written in JSON Schema's own vocabulary, which the draft 2020-12 meta-schema describes." },
    Opaque { scope: "command robot-docs schema", path: "envelope", source: "SchemaData.envelope", reason: "The envelope's published JSON Schema is written in JSON Schema's own vocabulary, which the draft 2020-12 meta-schema describes." },
    Opaque { scope: "command robot-docs schema", path: "events{}", source: "SchemaData.events", reason: "Each event's published JSON Schema is written in JSON Schema's own vocabulary, which the draft 2020-12 meta-schema describes." },
    Opaque { scope: "command robot-docs schema", path: "files{}", source: "SchemaData.files", reason: "Each file kind's published JSON Schema is written in JSON Schema's own vocabulary, which the draft 2020-12 meta-schema describes." },
    Opaque { scope: "command undo", path: "transactions[].error.details", source: "ItemError.details", reason: ITEM_ERROR_DETAILS },
    Opaque { scope: "envelope", path: "data", source: "Envelope.data", reason: "A command's data is described by its own schema, published per command, and validated against it." },
    Opaque { scope: "envelope", path: "error.details.items[].error.details", source: "ItemError.details", reason: ITEM_ERROR_DETAILS },
    Opaque { scope: "error", path: "details.items[].error.details", source: "ItemError.details", reason: ITEM_ERROR_DETAILS },
    Opaque { scope: "file doctor_report", path: "error.details.items[].error.details", source: "ItemError.details", reason: ITEM_ERROR_DETAILS },
    Opaque { scope: "file journal", path: "**.left_out[]", source: "LeftOutFolder.details", reason: LEFT_OUT_FOLDER },
    Opaque { scope: "file plan", path: "precondition.parts.command", source: "StoredParts.command", reason: PRECONDITION_PART },
    Opaque { scope: "file plan", path: "precondition.parts.observed", source: "StoredParts.observed", reason: PRECONDITION_PART },
    Opaque { scope: "file plan", path: "precondition.parts.selection", source: "StoredParts.selection", reason: PRECONDITION_PART },
    Opaque { scope: "file plan", path: "precondition.parts.targets", source: "StoredParts.targets", reason: PRECONDITION_PART },
    Opaque { scope: "file target_record", path: "**.left_out[]", source: "LeftOutFolder.details", reason: LEFT_OUT_FOLDER },
];

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::Path;

    use serde_json::Value;

    use super::*;
    use crate::contract::schema::{self, Visit, is_open, walk};
    use crate::source_scan::{Literals, production_code, sources};

    /// Every published schema with its scope.
    fn published() -> Vec<(String, Value)> {
        let mut out = vec![
            ("envelope".to_owned(), schema::envelope_schema().to_value()),
            ("error".to_owned(), schema::error_schema().to_value()),
        ];
        for name in crate::app::COMMANDS
            .iter()
            .map(|entry| entry.name)
            .chain(crate::app::ENVELOPE_ONLY.iter().map(|entry| entry.name))
        {
            let schema = schema::data_schema_for(name).unwrap().to_value();
            out.push((format!("command {name}"), schema));
        }
        for (name, schema) in schema::event_schemas() {
            out.push((format!("event {name}"), schema.to_value()));
        }
        for (kind, schema) in schema::file_schemas() {
            out.push((format!("file {kind}"), schema.to_value()));
        }
        out
    }

    /// Every open position of every published schema, as `(scope, path)`.
    fn open_positions() -> BTreeSet<(String, String)> {
        let mut open = BTreeSet::new();
        for (scope, schema) in published() {
            walk(&schema, &mut |visit| {
                if let Visit::Position { path, node } = visit
                    && is_open(node)
                {
                    open.insert((scope.clone(), (*path).to_owned()));
                }
            });
        }
        open
    }

    fn shown(path: &str) -> &str {
        if path.is_empty() { "." } else { path }
    }

    /// The open positions no row matches, and those more than one row matches.
    fn undeclared(
        rows: &[Opaque],
        open: &BTreeSet<(String, String)>,
    ) -> (Vec<String>, Vec<String>) {
        let mut unmatched = Vec::new();
        let mut ambiguous = Vec::new();
        for (scope, path) in open {
            let matching: Vec<_> = rows.iter().filter(|row| row.matches(scope, path)).collect();
            match matching.len() {
                1 => {}
                0 => unmatched.push(format!("{scope}: {}", shown(path))),
                _ => ambiguous.push(format!("{scope}: {} matches {matching:#?}", shown(path))),
            }
        }
        (unmatched, ambiguous)
    }

    /// The rows no open position matches.
    fn stale(rows: &[Opaque], open: &BTreeSet<(String, String)>) -> Vec<Opaque> {
        rows.iter()
            .filter(|row| !open.iter().any(|(scope, path)| row.matches(scope, path)))
            .copied()
            .collect()
    }

    #[test]
    fn the_registry_checks_catch_planted_faults() {
        let open: BTreeSet<_> = [("error", "details.x"), ("file plan", "a.b")]
            .map(|(scope, path)| (scope.to_owned(), path.to_owned()))
            .into();
        let row = |scope, path| Opaque {
            scope,
            path,
            source: "T.f",
            reason: "R.",
        };
        let declared = [row("error", "details.x"), row("file plan", "**.b")];
        assert_eq!(
            undeclared(&declared, &open),
            (vec![], vec![]),
            "all declared"
        );
        assert!(stale(&declared, &open).is_empty(), "none stale");
        let (unmatched, _) = undeclared(&declared[..1], &open);
        assert_eq!(unmatched, ["file plan: a.b"], "a missing row");
        let doubled = [declared[0], declared[1], row("file plan", "a.b")];
        let (_, ambiguous) = undeclared(&doubled, &open);
        assert_eq!(
            ambiguous.len(),
            1,
            "two rows for one position: {ambiguous:?}"
        );
        let extra = [declared[0], declared[1], row("error", "details.y")];
        assert_eq!(stale(&extra, &open), [extra[2]], "a row for no position");
    }

    #[test]
    fn every_open_position_is_declared_opaque() {
        let (unmatched, ambiguous) = undeclared(OPAQUE, &open_positions());
        assert!(
            unmatched.is_empty(),
            "open positions no row declares: add a `contract::OPAQUE` row with its reason, or \
             give the value a type:\n{}",
            unmatched.join("\n")
        );
        assert!(
            ambiguous.is_empty(),
            "open positions more than one row matches:\n{}",
            ambiguous.join("\n")
        );
    }

    #[test]
    fn every_opaque_row_matches_an_open_position() {
        let stale = stale(OPAQUE, &open_positions());
        assert!(
            stale.is_empty(),
            "stale `contract::OPAQUE` rows, matching no open position: {stale:#?}"
        );
    }

    #[test]
    fn opaque_rows_are_sorted_unique_and_reasoned() {
        for pair in OPAQUE.windows(2) {
            assert!(
                (pair[0].scope, pair[0].path) < (pair[1].scope, pair[1].path),
                "rows sorted by scope then path, no pair twice: {:?} before {:?}",
                pair[0],
                pair[1]
            );
        }
        let ident = |word: &str, first: fn(char) -> bool| {
            word.starts_with(first) && word.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        };
        for row in OPAQUE {
            let scope_ok = matches!(row.scope, "envelope" | "error")
                || ["command ", "event ", "file "]
                    .iter()
                    .any(|prefix| row.scope.len() > prefix.len() && row.scope.starts_with(prefix));
            assert!(scope_ok, "{row:?}: scope");
            assert!(!row.path.is_empty(), "{row:?}: path");
            let sentence = row.reason.strip_suffix('.').unwrap_or_default();
            assert!(
                sentence.starts_with(|c: char| c.is_ascii_uppercase())
                    && !sentence.contains(". ")
                    && !sentence.ends_with('.'),
                "{row:?}: the reason is one sentence ending with a period"
            );
            let (ty, field) = row.source.split_once('.').unwrap_or_default();
            assert!(
                ident(ty, |c| c.is_ascii_uppercase()) && ident(field, |c| c.is_ascii_lowercase()),
                "{row:?}: the source is `Type.field`"
            );
        }
    }

    fn ident_char(c: char) -> bool {
        c.is_alphanumeric() || c == '_'
    }

    /// Where `word` occurs in `text` as a whole identifier: the text before each.
    fn word_prefixes<'t>(text: &'t str, word: &'t str) -> impl Iterator<Item = &'t str> {
        text.match_indices(word).filter_map(move |(at, _)| {
            let before = &text[..at];
            (!before.ends_with(ident_char) && !text[at + word.len()..].starts_with(ident_char))
                .then_some(before)
        })
    }

    /// Whether `ty` names `serde_json::Value`, through the crate path or, in a file that
    /// imports it, as a bare `Value` (`toml::Value` is another type).
    fn mentions_value(ty: &str, imports_value: bool) -> bool {
        ty.contains("serde_json::Value")
            || ty.contains("serde_json::Map")
            || (imports_value && word_prefixes(ty, "Value").any(|before| !before.ends_with(':')))
    }

    /// Whether the file's `use serde_json…;` statements import `Value`.
    fn imports_value(code: &str) -> bool {
        code.match_indices("use serde_json").any(|(at, _)| {
            let statement = &code[at..];
            let statement = &statement[..statement.find(';').unwrap_or(statement.len())];
            word_prefixes(statement, "Value").next().is_some()
        })
    }

    /// `text` past a generic parameter list it starts with (`<'a, T: Fn() -> u8>`).
    fn past_generics(text: &str) -> &str {
        if !text.starts_with('<') {
            return text;
        }
        let mut depth = 0i32;
        let mut previous = ' ';
        for (at, c) in text.char_indices() {
            match c {
                '<' => depth += 1,
                '>' if previous != '-' => {
                    depth -= 1;
                    if depth == 0 {
                        return &text[at + 1..];
                    }
                }
                _ => {}
            }
            previous = c;
        }
        ""
    }

    /// The index just past the bracket closing the one at `open`.
    fn past_matching(chars: &[char], open: usize) -> usize {
        let mut depth = 0usize;
        for (i, c) in chars.iter().enumerate().skip(open) {
            match c {
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return i + 1;
                    }
                }
                _ => {}
            }
        }
        chars.len()
    }

    /// `text` split at the commas outside any bracket (`->` is no angle bracket).
    fn top_level_split(text: &str) -> Vec<String> {
        let chars: Vec<char> = text.chars().collect();
        let mut parts = vec![String::new()];
        let mut depth = 0i32;
        for (i, &c) in chars.iter().enumerate() {
            match c {
                '(' | '[' | '{' | '<' => depth += 1,
                '>' if i > 0 && chars[i - 1] == '-' => {}
                ')' | ']' | '}' | '>' => depth -= 1,
                ',' if depth == 0 => {
                    parts.push(String::new());
                    continue;
                }
                _ => {}
            }
            if let Some(last) = parts.last_mut() {
                last.push(c);
            }
        }
        parts
    }

    /// `text` with its leading attributes and visibility taken off, and whether a
    /// `#[serde(skip)]` or `#[serde(skip_serializing)]` was among them.
    fn strip_attributes(text: &str) -> (&str, bool) {
        let mut rest = text.trim_start();
        let mut skipped = false;
        while rest.starts_with("#[") {
            let chars: Vec<char> = rest.chars().collect();
            let end: usize = chars[..past_matching(&chars, 1)]
                .iter()
                .map(|c| c.len_utf8())
                .sum();
            let attribute = &rest[2..end - 1];
            if let Some(args) = attribute
                .trim()
                .strip_prefix("serde(")
                .and_then(|args| args.strip_suffix(')'))
            {
                skipped |= top_level_split(args)
                    .iter()
                    .any(|arg| matches!(arg.trim(), "skip" | "skip_serializing"));
            }
            rest = rest[end..].trim_start();
        }
        if let Some(after) = rest.strip_prefix("pub")
            && !after.starts_with(ident_char)
        {
            rest = after.trim_start();
            if rest.starts_with('(') {
                let chars: Vec<char> = rest.chars().collect();
                let end: usize = chars[..past_matching(&chars, 0)]
                    .iter()
                    .map(|c| c.len_utf8())
                    .sum();
                rest = rest[end..].trim_start();
            }
        }
        (rest, skipped)
    }

    /// The `name: Type` fields of a `{ … }` body naming `serde_json::Value`, as
    /// `owner.name`.
    fn value_fields(owner: &str, body: &str, imports_value: bool, out: &mut Vec<String>) {
        for field in top_level_split(body) {
            let (field, skipped) = strip_attributes(&field);
            let Some((name, ty)) = field.split_once(':') else {
                continue;
            };
            if !skipped && mentions_value(ty, imports_value) {
                out.push(format!("{owner}.{}", name.trim()));
            }
        }
    }

    /// `Type.field` for each field of a struct or struct-like enum variant that derives
    /// `Serialize` in `code` (production code, literals blank) and names
    /// `serde_json::Value`.
    fn serialized_value_fields(code: &str) -> Vec<String> {
        let imports_value = imports_value(code);
        let chars: Vec<char> = code.chars().collect();
        let text = |from: usize, to: usize| chars[from..to].iter().collect::<String>();
        let mut found = Vec::new();
        let mut at = 0;
        loop {
            let tail = text(at, chars.len());
            let Some(offset) = tail.find("#[derive(") else {
                break;
            };
            let start = at + tail[..offset].chars().count();
            let end = past_matching(&chars, start + 1);
            at = end;
            let derives = text(start + "#[derive(".len(), end - 2);
            if !derives
                .split(',')
                .any(|name| name.trim().rsplit("::").next() == Some("Serialize"))
            {
                continue;
            }
            let item = text(end, chars.len());
            let (item, _) = strip_attributes(&item);
            let (keyword, rest) = item.split_once(char::is_whitespace).unwrap_or_default();
            if !matches!(keyword, "struct" | "enum") {
                continue;
            }
            let rest = rest.trim_start();
            let name: String = rest.chars().take_while(|c| ident_char(*c)).collect();
            // A named body follows the generics, or a `where` clause; a tuple or unit
            // struct has none.
            let after = past_generics(rest[name.len()..].trim_start()).trim_start();
            let body = if after.starts_with('{') {
                after
            } else if after.starts_with("where") {
                &after[after.find(['{', ';']).unwrap_or(after.len())..]
            } else {
                ""
            };
            if !body.starts_with('{') {
                continue;
            }
            let body_chars: Vec<char> = body.chars().collect();
            let close = past_matching(&body_chars, 0);
            let body: String = body_chars[1..close - 1].iter().collect();
            if keyword == "struct" {
                value_fields(&name, &body, imports_value, &mut found);
            } else {
                for variant in top_level_split(&body) {
                    let (variant, _) = strip_attributes(&variant);
                    if let Some(open) = variant.find('{') {
                        let fields = &variant[open + 1..variant.rfind('}').unwrap_or(open + 1)];
                        value_fields(&name, fields, imports_value, &mut found);
                    }
                }
            }
        }
        found
    }

    #[test]
    fn the_value_field_scan_finds_each_shape() {
        let cases: &[(&str, &str, &[&str])] = &[
            (
                "plain_struct",
                "#[derive(Debug, Serialize)]\npub struct A { pub a: serde_json::Value, b: u8 }",
                &["A.a"],
            ),
            (
                "no_serialize",
                "#[derive(Debug, Deserialize)]\nstruct A { a: serde_json::Value }",
                &[],
            ),
            (
                "path_derive_and_option",
                "#[derive(serde::Serialize)]\nstruct A { a: Option<serde_json::Value> }",
                &["A.a"],
            ),
            (
                "bare_value_imported",
                "use serde_json::{Map, Value};\n#[derive(Serialize)]\nstruct A { m: Map<String, Value>, t: toml::Value }",
                &["A.m"],
            ),
            (
                "bare_value_not_imported",
                "#[derive(Serialize)]\nstruct A { v: Value }",
                &[],
            ),
            (
                "skipped_field",
                "#[derive(Serialize)]\nstruct A {\n    #[serde(skip)]\n    a: serde_json::Value,\n    #[serde(skip_serializing_if = \"   \")]\n    b: serde_json::Value,\n}",
                &["A.b"],
            ),
            (
                "attributes_visibility_and_generics",
                "#[derive(Serialize, JsonSchema)]\n#[serde(rename_all = \"   \")]\npub(crate) struct A<'a, T: Fn() -> u8> {\n    pub(crate) f: BTreeMap<String, (u8, serde_json::Value)>,\n    g: &'a T,\n}",
                &["A.f"],
            ),
            (
                "struct_like_variant",
                "#[derive(Serialize)]\nenum E { A { x: serde_json::Value }, B(serde_json::Value), C }",
                &["E.x"],
            ),
            (
                "tuple_struct",
                "#[derive(Serialize)]\nstruct T(serde_json::Value);\n#[derive(Serialize)]\nstruct U { u: serde_json::Value }",
                &["U.u"],
            ),
        ];
        for (name, code, expected) in cases {
            assert_eq!(
                serialized_value_fields(code),
                *expected,
                "case {name}: in\n{code}"
            );
        }
    }

    #[test]
    fn every_serialized_value_field_has_an_opaque_row() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut missing = Vec::new();
        for dir in ["src/app", "src/output", "src/contract", "src/doctor"] {
            let dir = root.join(dir);
            if !dir.is_dir() {
                continue;
            }
            for (path, text) in sources(&dir) {
                let Some(code) = production_code(&text, Literals::Blank) else {
                    continue;
                };
                for source in serialized_value_fields(&code) {
                    if !OPAQUE.iter().any(|row| row.source == source) {
                        let path = path.strip_prefix(root).unwrap_or(&path);
                        missing.push(format!("{}: {source}", path.display()));
                    }
                }
            }
        }
        assert!(
            missing.is_empty(),
            "serialized `serde_json::Value` fields no `contract::OPAQUE` row names as its \
             source: add the row, or give the value a type:\n{}",
            missing.join("\n")
        );
    }

    #[test]
    fn the_known_value_fields_are_found() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut found = BTreeSet::new();
        for dir in ["src/app", "src/output"] {
            for (_, text) in sources(&root.join(dir)) {
                if let Some(code) = production_code(&text, Literals::Blank) {
                    found.extend(serialized_value_fields(&code));
                }
            }
        }
        for source in [
            "ItemError.details",
            "Setting.value",
            "GetData.value",
            "GetData.default",
            "SetData.value",
            "SetData.previous",
        ] {
            assert!(found.contains(source), "{source}: not found in {found:?}");
        }
    }

    #[test]
    fn rows_match_by_scope_and_path() {
        let row = |scope, path| Opaque {
            scope,
            path,
            source: "T.f",
            reason: "R.",
        };
        let cases = [
            (
                "exact",
                row("error", "details.x"),
                "error",
                "details.x",
                true,
            ),
            (
                "other_scope",
                row("error", "details.x"),
                "envelope",
                "details.x",
                false,
            ),
            (
                "other_path",
                row("error", "details.x"),
                "error",
                "details.y",
                false,
            ),
            (
                "every_command",
                row("command *", "a"),
                "command config get",
                "a",
                true,
            ),
            (
                "every_command_not_a_file",
                row("command *", "a"),
                "file history",
                "a",
                false,
            ),
            (
                "any_depth_at_root",
                row("file x", "**.a[]"),
                "file x",
                "a[]",
                true,
            ),
            (
                "any_depth_nested",
                row("file x", "**.a[]"),
                "file x",
                "b{}.c.a[]",
                true,
            ),
            (
                "any_depth_whole_segment",
                row("file x", "**.a[]"),
                "file x",
                "ba[]",
                false,
            ),
            (
                "any_depth_suffix_only",
                row("file x", "**.a[]"),
                "file x",
                "a[].b",
                false,
            ),
        ];
        for (name, row, scope, path, expected) in cases {
            assert_eq!(
                row.matches(scope, path),
                expected,
                "case {name}: {row:?} at {scope}: {path}"
            );
        }
    }
}
