//! Node ids and the position paths inside them. Every id begins with its section word, so
//! the readable projection's leading tokens name it and `parent` needs no schema.
//!
//! A position is a root token followed by segments: `data` for a command's data (the
//! envelope key it lives under), `.` for every other surface. Segments are `.key`, `[]`
//! (array items), `[0]` (`prefixItems`), `{}` (map values), `<tag=value>` (a tagged
//! variant) and `<untagged=k1,k2|o1,o2>` (an untagged one: its required, then optional
//! keys). The first key after a `.` root is written bare (`envelope meta`). A key or tag
//! value that is not `[A-Za-z0-9_-]+` is written as a JSON string, so no key can forge a
//! segment. These functions are the only way paths are built.

use serde_json::Value;

/// The root position of every surface but a command's data.
pub const ROOT: &str = ".";
/// The root token of a command's data: the envelope key it lives under.
pub const DATA: &str = "data";

/// The section words that begin ids.
pub mod section {
    pub const COMMAND: &str = "command";
    pub const INPUT: &str = "input";
    pub const OUTPUT: &str = "output";
    pub const ENVELOPE: &str = "envelope";
    pub const EVENT: &str = "event";
    pub const ERROR: &str = "error";
    pub const FILE: &str = "file";
    pub const CONFIG: &str = "config";
    pub const DESIRED: &str = "desired";
    pub const ENV: &str = "env";
    pub const WARNING: &str = "warning";
    pub const ACTION: &str = "action";
    pub const FAILURE_MODE: &str = "fm";
    pub const PHASE: &str = "phase";
    pub const LOG_LEVEL: &str = "log-level";
    pub const EXIT: &str = "exit";
    pub const DOCTOR_OPS: &str = "doctor-ops";
}

/// `command <path>`; the root is `command omaddons`.
pub fn command(path: &str) -> String {
    format!("{} {path}", section::COMMAND)
}

/// `input <path> --<long>`.
pub fn flag(path: &str, long: &str) -> String {
    format!("{} {path} --{long}", section::INPUT)
}

/// `input <path> -<short>`, for a flag without a long name.
pub fn short_flag(path: &str, short: char) -> String {
    format!("{} {path} -{short}", section::INPUT)
}

/// `input <path> @<index>`, clap's 1-based index.
pub fn positional(path: &str, index: usize) -> String {
    format!("{} {path} @{index}", section::INPUT)
}

/// `output <path> <position>`; `position` starts at [`DATA`].
pub fn output(path: &str, position: &str) -> String {
    format!("{} {path} {position}", section::OUTPUT)
}

/// `envelope <position>`.
pub fn envelope(position: &str) -> String {
    format!("{} {position}", section::ENVELOPE)
}

/// `desired <position>`.
pub fn desired(position: &str) -> String {
    format!("{} {position}", section::DESIRED)
}

/// An anchored surface: `event <name>`, `error <code>`, `file <kind>`, and with a position
/// `event <name> <position>`.
pub fn anchored(section: &str, name: &str, position: Option<&str>) -> String {
    match position {
        Some(position) => format!("{section} {name} {position}"),
        None => format!("{section} {name}"),
    }
}

/// A registry node with no position: `warning <code>`, `config <key>`, `env <NAME>`,
/// `exit general <n>`.
pub fn registry(section: &str, name: &str) -> String {
    format!("{section} {name}")
}

/// `key`, quoted as a JSON string unless it is `[A-Za-z0-9_-]+`.
pub fn quote(key: &str) -> String {
    let safe = !key.is_empty()
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if safe {
        key.to_owned()
    } else {
        Value::from(key).to_string()
    }
}

/// `path.key`; after a `.` root, the key alone.
pub fn push_key(path: &str, key: &str) -> String {
    if path == ROOT {
        quote(key)
    } else {
        format!("{path}.{}", quote(key))
    }
}

/// `path[]`: an array's items.
pub fn push_items(path: &str) -> String {
    join(path, "[]")
}

/// `path[n]`: a `prefixItems` member.
pub fn push_index(path: &str, n: usize) -> String {
    join(path, &format!("[{n}]"))
}

/// `path{}`: a map's values.
pub fn push_map(path: &str) -> String {
    join(path, "{}")
}

/// `path<tag=value>`: a tagged variant.
pub fn push_variant(path: &str, tag: &str, value: &str) -> String {
    join(path, &format!("<{}={}>", quote(tag), quote(value)))
}

/// `path<untagged=k1,k2|o1,o2>`: an untagged variant by its required and optional keys,
/// each list sorted; `|` and the optional keys only when there are some.
pub fn push_untagged(path: &str, required: &[&str], optional: &[&str]) -> String {
    let list = |keys: &[&str]| {
        let mut keys: Vec<&str> = keys.to_vec();
        keys.sort_unstable();
        keys.dedup();
        keys.into_iter().map(quote).collect::<Vec<_>>().join(",")
    };
    let mut segment = format!("<untagged={}", list(required));
    if !optional.is_empty() {
        segment.push('|');
        segment.push_str(&list(optional));
    }
    segment.push('>');
    join(path, &segment)
}

/// `path` followed by `segment`; a `.` root is replaced by it.
fn join(path: &str, segment: &str) -> String {
    if path == ROOT {
        segment.to_owned()
    } else {
        format!("{path}{segment}")
    }
}

/// An id's first token.
pub fn section(id: &str) -> &str {
    id.split_once(' ').map_or(id, |(section, _)| section)
}

/// The id with its last segment removed. A surface root's parent is its anchor
/// (`output <path> data` → `command <path>`, `event <name> .` → `event <name>`); an
/// argument's is `command <path>`; commands, `envelope .`, `desired .`, anchors and
/// registry nodes have none.
pub fn parent(id: &str) -> Option<String> {
    let (head, rest) = id.split_once(' ')?;
    match head {
        section::INPUT => {
            let end = [rest.find(" -"), rest.find(" @")]
                .into_iter()
                .flatten()
                .min()?;
            Some(command(&rest[..end]))
        }
        section::OUTPUT => {
            let at = data_start(rest)?;
            let (path, position) = (&rest[..at], &rest[at + 1..]);
            let up = strip_last(position);
            Some(if up.is_empty() {
                command(path)
            } else {
                output(path, up)
            })
        }
        section::ENVELOPE | section::DESIRED => {
            (rest != ROOT).then(|| format!("{head} {}", or_root(strip_last(rest))))
        }
        section::EVENT | section::ERROR | section::FILE => {
            let (name, position) = rest.split_once(' ')?;
            Some(if position == ROOT {
                anchored(head, name, None)
            } else {
                anchored(head, name, Some(or_root(strip_last(position))))
            })
        }
        _ => None,
    }
}

fn or_root(position: &str) -> &str {
    if position.is_empty() { ROOT } else { position }
}

/// Where ` data` begins the position of an `output` id's rest (`backup restore data.x`):
/// the command path ends there.
fn data_start(rest: &str) -> Option<usize> {
    rest.match_indices(" data").map(|(at, _)| at).find(|at| {
        rest[at + " data".len()..]
            .chars()
            .next()
            .is_none_or(|c| matches!(c, '.' | '[' | '{' | '<'))
    })
}

/// `position` without its last segment; empty when it has one segment.
fn strip_last(position: &str) -> &str {
    let last = segment_starts(position).last().copied().unwrap_or(0);
    &position[..last]
}

/// The byte offsets where `position`'s segments start. Quoted keys and values are skipped
/// whole, so a quoted `.`, `[` or `>` starts nothing.
fn segment_starts(position: &str) -> Vec<usize> {
    let bytes = position.as_bytes();
    let mut starts = vec![0];
    let (mut quoted, mut variant) = (false, false);
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if quoted {
            match b {
                b'\\' => i += 1,
                b'"' => quoted = false,
                _ => {}
            }
        } else {
            match b {
                b'"' => quoted = true,
                b'>' if variant => variant = false,
                b'<' if !variant => {
                    variant = true;
                    if i > 0 {
                        starts.push(i);
                    }
                }
                b'.' | b'[' | b'{' if !variant && i > 0 => starts.push(i),
                _ => {}
            }
        }
        i += 1;
    }
    starts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_quote_unsafe_keys_and_tag_values() {
        let cases = [
            ("plain", push_key(DATA, "items"), "data.items"),
            ("dotted_key", push_key(DATA, "a.b"), r#"data."a.b""#),
            (
                "space_in_tag_value",
                push_variant(DATA, "type", "x y"),
                r#"data<type="x y">"#,
            ),
            ("bracket_in_key", push_key(DATA, "a[0]"), r#"data."a[0]""#),
            ("angle_in_key", push_key(DATA, "a<b"), r#"data."a<b""#),
            ("empty_key", push_key(DATA, ""), r#"data."""#),
            ("first_key_after_root", push_key(ROOT, "meta"), "meta"),
            ("quoted_first_key", push_key(ROOT, "a b"), r#""a b""#),
            ("quote_in_key", push_key(DATA, "a\"b"), r#"data."a\"b""#),
        ];
        for (case, got, want) in cases {
            assert_eq!(got, want, "{case}: gave {got:?}, expected {want:?}");
        }
    }

    #[test]
    fn segments_after_each_root() {
        let items = push_items(&push_key(DATA, "items"));
        let cases = [
            ("items", items.clone(), "data.items[]"),
            (
                "key_after_items",
                push_key(&items, "status"),
                "data.items[].status",
            ),
            (
                "index",
                push_index(&push_key(DATA, "pair"), 1),
                "data.pair[1]",
            ),
            ("map", push_map(&push_key(ROOT, "family")), "family{}"),
            (
                "variant_at_root",
                push_variant(ROOT, "type", "step"),
                "<type=step>",
            ),
            ("items_at_root", push_items(ROOT), "[]"),
            (
                "untagged",
                push_untagged(DATA, &["b", "a", "b"], &[]),
                "data<untagged=a,b>",
            ),
            (
                "untagged_optional",
                push_untagged(ROOT, &["k"], &["z", "o"]),
                "<untagged=k|o,z>",
            ),
            (
                "untagged_quoted",
                push_untagged(ROOT, &["a,b"], &[]),
                r#"<untagged="a,b">"#,
            ),
        ];
        for (case, got, want) in cases {
            assert_eq!(got, want, "{case}: gave {got:?}, expected {want:?}");
        }
    }

    #[test]
    fn parent_of_each_id_form() {
        let cases: [(&str, &str, Option<&str>); 22] = [
            (
                "nested_key",
                "output backup restore data.files.restored",
                Some("output backup restore data.files"),
            ),
            (
                "items",
                "output install data.items[]",
                Some("output install data.items"),
            ),
            ("data_root", "output install data", Some("command install")),
            (
                "variant",
                "output install data.result<command=install>",
                Some("output install data.result"),
            ),
            ("envelope_key", "envelope meta", Some("envelope .")),
            (
                "envelope_nested",
                "envelope meta.elapsed_ms",
                Some("envelope meta"),
            ),
            ("envelope_root", "envelope .", None),
            (
                "event_key",
                "event progress phase",
                Some("event progress ."),
            ),
            ("event_root", "event progress .", Some("event progress")),
            ("event_anchor", "event progress", None),
            ("error_root", "error plan_stale .", Some("error plan_stale")),
            (
                "error_nested",
                "error plan_stale details.changed[]",
                Some("error plan_stale details.changed"),
            ),
            (
                "file_variant",
                "file journal <type=step>",
                Some("file journal ."),
            ),
            (
                "file_quoted_variant",
                r#"file journal <type="a>b.c">"#,
                Some("file journal ."),
            ),
            ("positional", "input undo @1", Some("command undo")),
            (
                "global_flag",
                "input omaddons --json",
                Some("command omaddons"),
            ),
            (
                "nested_short_flag",
                "input backup restore -t",
                Some("command backup restore"),
            ),
            (
                "quoted_key",
                r#"output x data."a.b""#,
                Some("output x data"),
            ),
            ("command", "command backup restore", None),
            ("desired_map", "desired family{}", Some("desired family")),
            ("desired_key", "desired family", Some("desired .")),
            ("registry", "warning x", None),
        ];
        for (case, input, want) in cases {
            let got = parent(input);
            assert_eq!(
                got.as_deref(),
                want,
                "{case}: input {input:?} gave {got:?}, expected {want:?}"
            );
        }
    }

    #[test]
    fn constructors_spell_each_form() {
        let cases = [
            (
                "command",
                command("backup restore"),
                "command backup restore",
            ),
            ("flag", flag("omaddons", "json"), "input omaddons --json"),
            ("short_flag", short_flag("install", 'v'), "input install -v"),
            ("positional", positional("undo", 1), "input undo @1"),
            ("output", output("install", DATA), "output install data"),
            ("envelope", envelope(ROOT), "envelope ."),
            ("desired", desired("family"), "desired family"),
            (
                "event",
                anchored(section::EVENT, "progress", None),
                "event progress",
            ),
            (
                "event_position",
                anchored(section::EVENT, "progress", Some(ROOT)),
                "event progress .",
            ),
            (
                "exit",
                registry(section::EXIT, "general 0"),
                "exit general 0",
            ),
        ];
        for (case, got, want) in cases {
            assert_eq!(got, want, "{case}: gave {got:?}, expected {want:?}");
            assert_eq!(
                section(&got),
                want.split(' ').next().unwrap(),
                "{case}: section"
            );
        }
        assert_eq!(
            section("command"),
            "command",
            "a one-word id is its section"
        );
    }
}
