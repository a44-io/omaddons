//! `omaddons robot-docs`: documentation for programs and agents, generated from the code
//! (M4-D2). `guide` (and bare `robot-docs`) prints the agent guide, `schema [<command>]`
//! publishes `contract::schema`'s JSON Schemas unchanged, and `errors` the error and
//! warning catalog. None reads a file, needs `HOME`, sends a request or writes anything.

use std::collections::BTreeMap;
use std::io::Write;

use clap::CommandFactory;
use schemars::{JsonSchema, Schema};
use serde::Serialize;

use super::{COMMANDS, CommandEntry, command_entry};
use crate::cli::{Cli, levenshtein};
use crate::contract::schema::{self, SchemaDoc};
use crate::error::{Error, ErrorCode, Result, Retryability, Suggestion, WarningCode};
use crate::output::{Outcome, Style, table};

/// `robot-docs`' arguments.
#[derive(Debug, Clone, clap::Args)]
pub struct RobotDocsArgs {
    #[command(subcommand)]
    pub action: Option<RobotDocsAction>,
}

/// What `robot-docs` documents.
#[derive(Debug, Clone, clap::Subcommand)]
pub enum RobotDocsAction {
    /// The agent guide as Markdown: the same text as the skill
    #[command(after_long_help = GUIDE_HELP)]
    Guide,
    /// The JSON Schema of every command's data, the envelope, the stderr events and the
    /// files, or of one command's data
    Schema(SchemaArgs),
    /// Every error and warning code: exit, retryability, meaning, details schema and fix
    /// pattern
    Errors,
}

impl RobotDocsAction {
    /// The envelope `command`.
    pub fn command(&self) -> &'static str {
        match self {
            Self::Guide => GUIDE_COMMAND,
            Self::Schema(_) => SCHEMA_COMMAND,
            Self::Errors => ERRORS_COMMAND,
        }
    }
}

/// The envelope `command` of `robot-docs guide` and of bare `robot-docs`.
pub const GUIDE_COMMAND: &str = "robot-docs guide";
const SCHEMA_COMMAND: &str = "robot-docs schema";
const ERRORS_COMMAND: &str = "robot-docs errors";

const GUIDE_HELP: &str = "\
The guide reads no file or setting, so it prints the same bytes on every machine:
the same text as skills/omaddons/SKILL.md, opening with YAML frontmatter. With
--json, data is {\"format\":\"markdown\",\"content\":\"...\"}.";

/// The format of a `robot-docs` document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DocFormat {
    Markdown,
}

/// `robot-docs guide`'s data: the guide, whole.
#[derive(Debug, Serialize, JsonSchema)]
pub struct GuideData {
    pub format: DocFormat,
    /// The guide's text: YAML frontmatter, its stamp, then Markdown.
    pub content: String,
}

#[derive(Debug)]
pub struct GuideOutcome {
    data: GuideData,
}

impl Outcome for GuideOutcome {
    type Data = GuideData;

    fn command(&self) -> &'static str {
        GUIDE_COMMAND
    }

    fn data(&self) -> &GuideData {
        &self.data
    }

    fn render_human(&self, w: &mut dyn Write, _style: &Style) -> std::io::Result<()> {
        w.write_all(self.data.content.as_bytes())
    }
}

/// `robot-docs guide` and bare `robot-docs`: the embedded guide, rendered from the
/// registries. It reads no file and writes nothing.
pub fn guide() -> Result<GuideOutcome> {
    let content = crate::contract::guide::render_guide()
        .map_err(|e| Error::internal(format!("the embedded guide does not render: {e}")))?;
    Ok(GuideOutcome {
        data: GuideData {
            format: DocFormat::Markdown,
            content,
        },
    })
}

/// `robot-docs schema`'s arguments.
#[derive(Debug, Clone, Default, clap::Args)]
pub struct SchemaArgs {
    /// A command path, as omaddons help lists it: install, or backup restore as two words
    /// or one quoted
    #[arg(value_name = "COMMAND", num_args = 0..)]
    pub command: Vec<String>,
}

/// `robot-docs schema`: published JSON Schemas (draft 2020-12, serialize contract, one
/// generator each).
#[derive(Debug, Serialize, JsonSchema)]
pub struct SchemaData {
    /// The envelope's schema; `null` when one command was asked for.
    pub envelope: Option<SchemaDoc>,
    /// One schema per stderr NDJSON event, by event name; `null` when one command was
    /// asked for.
    pub events: Option<BTreeMap<String, SchemaDoc>>,
    /// The schema of each command's `data`, by command path: every path, or the one asked
    /// for.
    pub commands: BTreeMap<String, SchemaDoc>,
    /// One schema per file kind omaddons writes (a JSON Lines kind: one line); `null` when
    /// one command was asked for.
    pub files: Option<BTreeMap<String, SchemaDoc>>,
}

/// What `robot-docs schema` returns.
#[derive(Debug)]
pub struct SchemaOutcome {
    data: SchemaData,
}

impl Outcome for SchemaOutcome {
    type Data = SchemaData;

    fn command(&self) -> &'static str {
        SCHEMA_COMMAND
    }

    fn data(&self) -> &SchemaData {
        &self.data
    }

    /// The data itself, pretty: a schema has no better human rendering.
    fn render_human(&self, w: &mut dyn Write, _style: &Style) -> std::io::Result<()> {
        let text = serde_json::to_string_pretty(&self.data).map_err(std::io::Error::other)?;
        writeln!(w, "{text}")
    }
}

/// `omaddons robot-docs schema [<command>]`.
pub fn schema(words: &[String]) -> Result<SchemaOutcome> {
    let keyed = |map: BTreeMap<&'static str, Schema>| -> BTreeMap<String, SchemaDoc> {
        map.into_iter()
            .map(|(key, schema)| (key.to_owned(), SchemaDoc(schema)))
            .collect()
    };
    let data = match resolve(words)? {
        None => SchemaData {
            envelope: Some(SchemaDoc(schema::envelope_schema())),
            events: Some(keyed(schema::event_schemas())),
            commands: keyed(schema::command_schemas()),
            files: Some(keyed(schema::file_schemas())),
        },
        Some(entry) => SchemaData {
            envelope: None,
            events: None,
            commands: BTreeMap::from([(
                entry.name.to_owned(),
                SchemaDoc(schema::command_schema(entry)),
            )]),
            files: None,
        },
    };
    Ok(SchemaOutcome { data })
}

/// The `COMMANDS` row `words` name (each split on whitespace), or `None` for the full
/// listing. Aliases name their command. An unknown path is `usage`, its fix the closest
/// path, else the row a prefix walked to, else `help robot-docs schema`.
fn resolve(words: &[String]) -> Result<Option<&'static CommandEntry>> {
    let words: Vec<&str> = words.iter().flat_map(|w| w.split_whitespace()).collect();
    if words.is_empty() {
        return Ok(None);
    }
    let mut root = Cli::command();
    root.build();
    let mut names: Vec<String> = Vec::new();
    let mut walked_row = None;
    let mut level = &root;
    for word in &words {
        match level.find_subcommand(word) {
            Some(sub) => {
                names.push(sub.get_name().to_owned());
                if let Some(row) = command_entry(&names.join(" ")) {
                    walked_row = Some(row);
                }
                level = sub;
            }
            None => return Err(unknown(&words, walked_row, &root)),
        }
    }
    let path = names.join(" ");
    if let Some(row) = command_entry(&path) {
        return Ok(Some(row));
    }
    let under: Vec<&CommandEntry> = COMMANDS
        .iter()
        .filter(|row| row.name.starts_with(&format!("{path} ")))
        .collect();
    let listed: Vec<String> = under.iter().map(|row| format!("`{}`", row.name)).collect();
    let message = format!(
        "`{path}` has no schema of its own: its commands are {}",
        listed.join(", ")
    );
    Err(usage_to(message, &under))
}

/// `usage` for a path no command has.
fn unknown(words: &[&str], walked: Option<&'static CommandEntry>, root: &clap::Command) -> Error {
    let typed = words.join(" ");
    let mut close: Vec<(usize, &'static str)> = COMMANDS
        .iter()
        .map(|row| (levenshtein(&typed, row.name), row.name))
        .filter(|(distance, _)| *distance <= 2)
        .collect();
    for sub in root.get_subcommands() {
        let Some(row) = command_entry(sub.get_name()) else {
            continue;
        };
        for alias in sub.get_visible_aliases() {
            let distance = levenshtein(&typed, alias);
            if distance <= 2 {
                close.push((distance, row.name));
            }
        }
    }
    close.sort_unstable();
    let mut rows: Vec<&'static CommandEntry> = Vec::new();
    for (_, name) in close {
        if let Some(row) = command_entry(name).filter(|row| rows.iter().all(|r| r.name != row.name))
        {
            rows.push(row);
        }
    }
    if rows.is_empty()
        && let Some(row) = walked
    {
        rows.push(row);
    }
    let mut message = format!("there is no command {typed:?} to describe");
    match rows.first() {
        Some(best) => {
            message.push_str(&format!("; the closest is {}", best.name));
            usage_to(message, &rows)
        }
        None => Error::new(
            ErrorCode::Usage,
            message,
            Suggestion::command(["omaddons", "help", "robot-docs", "schema"]),
        ),
    }
}

/// `usage` whose fix is `robot-docs schema <first row>`, one alternative per other row.
fn usage_to(message: String, rows: &[&CommandEntry]) -> Error {
    let line = |row: &CommandEntry| {
        let mut argv = vec!["omaddons", "robot-docs", "schema"];
        argv.extend(row.name.split(' '));
        Suggestion::command(argv)
    };
    let fix = rows.first().map_or_else(
        || Suggestion::command(["omaddons", "help", "robot-docs", "schema"]),
        |row| line(row),
    );
    let mut error = Error::new(ErrorCode::Usage, message, fix).user_verb();
    for row in rows.iter().skip(1) {
        error = error.alternative(line(row));
    }
    error
}

/// `robot-docs errors`: the code registry.
#[derive(Debug, Serialize, JsonSchema)]
pub struct ErrorsData {
    /// Every error code, in registry order.
    pub error_codes: Vec<ErrorCodeDoc>,
    /// Every warning code, in registry order (a mirrored item failure carries its error's
    /// code, listed above).
    pub warning_codes: Vec<WarningCodeDoc>,
}

/// One error code.
#[derive(Debug, Serialize, JsonSchema)]
pub struct ErrorCodeDoc {
    pub code: ErrorCode,
    /// The general exit code of every command.
    pub exit: u8,
    /// The exit code of every `doctor` command, from the doctor's own dictionary.
    pub doctor_exit: u8,
    /// `true`, `false`, or `"conditional"` when each error's own `retryable` decides.
    pub retryable: Retryability,
    /// What the code reports, in one sentence.
    pub meaning: &'static str,
    /// The code's typed `details`, as a JSON Schema fragment.
    pub details_schema: SchemaDoc,
    /// What form the fix takes.
    pub fix_pattern: &'static str,
}

/// One warning code.
#[derive(Debug, Serialize, JsonSchema)]
pub struct WarningCodeDoc {
    pub code: WarningCode,
    /// What the warning reports, in one sentence.
    pub meaning: &'static str,
    /// What form the fix takes.
    pub fix_pattern: &'static str,
}

/// What `robot-docs errors` returns.
#[derive(Debug)]
pub struct ErrorsOutcome {
    data: ErrorsData,
}

impl Outcome for ErrorsOutcome {
    type Data = ErrorsData;

    fn command(&self) -> &'static str {
        ERRORS_COMMAND
    }

    fn data(&self) -> &ErrorsData {
        &self.data
    }

    /// Two tables; `-v` adds each code's fix pattern on its own line under it, so a long
    /// pattern never widens a column.
    fn render_human(&self, w: &mut dyn Write, style: &Style) -> std::io::Result<()> {
        let retryable = |r: Retryability| match r {
            Retryability::Fixed(true) => "yes",
            Retryability::Fixed(false) => "no",
            Retryability::Conditional(_) => "conditional",
        };
        let mut rows = vec![
            ["CODE", "EXIT", "DOCTOR", "RETRYABLE", "MEANING"]
                .map(str::to_owned)
                .to_vec(),
        ];
        rows.extend(self.data.error_codes.iter().map(|doc| {
            vec![
                doc.code.wire().to_owned(),
                doc.exit.to_string(),
                doc.doctor_exit.to_string(),
                retryable(doc.retryable).to_owned(),
                doc.meaning.to_owned(),
            ]
        }));
        let patterns: Vec<&str> = self
            .data
            .error_codes
            .iter()
            .map(|d| d.fix_pattern)
            .collect();
        let heading = format!("Error codes ({})", self.data.error_codes.len());
        section(w, style, &heading, &rows, &patterns)?;
        writeln!(w)?;
        let mut rows = vec![["CODE", "MEANING"].map(str::to_owned).to_vec()];
        rows.extend(
            self.data
                .warning_codes
                .iter()
                .map(|doc| vec![doc.code.wire().to_owned(), doc.meaning.to_owned()]),
        );
        let patterns: Vec<&str> = self
            .data
            .warning_codes
            .iter()
            .map(|d| d.fix_pattern)
            .collect();
        let heading = format!("Warning codes ({})", self.data.warning_codes.len());
        section(w, style, &heading, &rows, &patterns)
    }
}

/// A heading and its table; under `-v`, each row's fix pattern after it.
fn section(
    w: &mut dyn Write,
    style: &Style,
    heading: &str,
    rows: &[Vec<String>],
    patterns: &[&str],
) -> std::io::Result<()> {
    writeln!(w, "{}", style.bold(heading))?;
    for (index, line) in table(rows).lines().enumerate() {
        writeln!(w, "{line}")?;
        if style.paths_visible()
            && let Some(pattern) = index.checked_sub(1).and_then(|i| patterns.get(i))
        {
            writeln!(w, "      fix: {pattern}")?;
        }
    }
    Ok(())
}

/// `omaddons robot-docs errors`.
pub fn errors() -> Result<ErrorsOutcome> {
    let mut error_codes = Vec::with_capacity(ErrorCode::ALL.len());
    for code in ErrorCode::ALL {
        let details = Schema::try_from(code.details_schema()).map_err(|e| {
            Error::internal(format!("{code}: its details schema is no schema: {e}"))
        })?;
        error_codes.push(ErrorCodeDoc {
            code: *code,
            exit: code.exit().code(),
            doctor_exit: crate::doctor::exit::exit_for(*code).code(),
            retryable: code.retryability(),
            meaning: code.meaning(),
            details_schema: SchemaDoc(details),
            fix_pattern: code.fix_pattern(),
        });
    }
    let warning_codes = WarningCode::ALL
        .iter()
        .map(|code| WarningCodeDoc {
            code: *code,
            meaning: code.meaning(),
            fix_pattern: code.fix_pattern(),
        })
        .collect();
    Ok(ErrorsOutcome {
        data: ErrorsData {
            error_codes,
            warning_codes,
        },
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use clap::Parser as _;
    use serde_json::Value;

    use super::*;
    use crate::contract::events::EventName;

    fn words(text: &[&str]) -> Vec<String> {
        text.iter().map(|w| (*w).to_owned()).collect()
    }

    fn rendered(outcome: &impl Outcome, style: &Style) -> String {
        let mut out = Vec::new();
        outcome.render_human(&mut out, style).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn schema_listing_publishes_every_registry_entry() {
        let data = schema(&[]).unwrap().data;
        let commands: Vec<&str> = data.commands.keys().map(String::as_str).collect();
        let mut expected: Vec<&str> = COMMANDS.iter().map(|row| row.name).collect();
        expected.sort_unstable();
        assert_eq!(
            commands, expected,
            "commands are the COMMANDS paths, sorted"
        );
        let events: BTreeSet<&str> = data
            .events
            .as_ref()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(events, EventName::ALL.iter().map(|e| e.wire()).collect());
        let files: BTreeSet<&str> = data
            .files
            .as_ref()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(files, schema::file_schemas().keys().copied().collect());
        assert!(data.envelope.is_some());
    }

    #[test]
    fn schema_listing_entries_equal_the_generators() {
        let data = schema(&[]).unwrap().data;
        let same = |label: String, got: &SchemaDoc, expected: Schema| {
            assert_eq!(
                serde_json::to_value(got).unwrap(),
                serde_json::to_value(expected).unwrap(),
                "{label}"
            );
        };
        same(
            "envelope".into(),
            data.envelope.as_ref().unwrap(),
            schema::envelope_schema(),
        );
        for row in COMMANDS {
            same(
                format!("command {}", row.name),
                &data.commands[row.name],
                schema::command_schema(row),
            );
        }
        for (name, expected) in schema::event_schemas() {
            same(
                format!("event {name}"),
                &data.events.as_ref().unwrap()[name],
                expected,
            );
        }
        for (kind, expected) in schema::file_schemas() {
            same(
                format!("file {kind}"),
                &data.files.as_ref().unwrap()[kind],
                expected,
            );
        }
    }

    /// The fix line `omaddons robot-docs schema <path>`.
    fn schema_fix(path: &str) -> Vec<String> {
        let mut argv = words(&["omaddons", "robot-docs", "schema"]);
        argv.extend(path.split(' ').map(str::to_owned));
        argv
    }

    /// A resolution case's expectation: the row's path (`None` for the listing), or the
    /// usage error's message prefix and fix.
    type Expected = std::result::Result<Option<&'static str>, (&'static str, Vec<String>)>;

    #[test]
    fn resolve_path_cases() {
        let rowless_parent = command_entry("auth").is_none();
        let mut cases: Vec<(&str, Vec<String>, Expected)> = vec![
            ("none", vec![], Ok(None)),
            ("install", words(&["install"]), Ok(Some("install"))),
            ("alias_add", words(&["add"]), Ok(Some("install"))),
            (
                "two_words",
                words(&["backup", "restore"]),
                Ok(Some("backup restore")),
            ),
            (
                "one_quoted_word",
                words(&["backup restore"]),
                Ok(Some("backup restore")),
            ),
            (
                "extra_spaces",
                words(&["backup   restore"]),
                Ok(Some("backup restore")),
            ),
            ("parent_row_backup", words(&["backup"]), Ok(Some("backup"))),
            (
                "itself",
                words(&["robot-docs", "schema"]),
                Ok(Some("robot-docs schema")),
            ),
            (
                "typo_instal",
                words(&["instal"]),
                Err((
                    "there is no command \"instal\" to describe; the closest is install",
                    schema_fix("install"),
                )),
            ),
            (
                "typo_nested",
                words(&["backup", "restor"]),
                Err((
                    "there is no command \"backup restor\" to describe; the closest is backup restore",
                    schema_fix("backup restore"),
                )),
            ),
            (
                "trailing_word",
                words(&["install", "extra"]),
                Err((
                    "there is no command \"install extra\" to describe; the closest is install",
                    schema_fix("install"),
                )),
            ),
            (
                "nothing_close",
                words(&["nosuch"]),
                Err((
                    "there is no command \"nosuch\" to describe",
                    words(&["omaddons", "help", "robot-docs", "schema"]),
                )),
            ),
        ];
        if rowless_parent {
            cases.push((
                "rowless_parent_auth",
                words(&["auth"]),
                Err(("`auth` has no schema of its own", schema_fix("auth github"))),
            ));
        }
        for (name, input, expected) in cases {
            let got = resolve(&input);
            match (&got, &expected) {
                (Ok(row), Ok(path)) => {
                    assert_eq!(row.map(|r| r.name), *path, "case {name}: {input:?}");
                }
                (Err(error), Err((prefix, fix))) => {
                    assert_eq!(error.code, ErrorCode::Usage, "case {name}: {input:?}");
                    assert!(
                        error.message.starts_with(prefix),
                        "case {name}: {:?}",
                        error.message
                    );
                    assert_eq!(
                        error.fix.argv(),
                        Some(fix.as_slice()),
                        "case {name}: {input:?}"
                    );
                    assert_eq!(error.details, serde_json::json!({}), "case {name}");
                    assert!(!error.retryable, "case {name}");
                }
                _ => panic!("case {name}: {input:?}: got {got:?}"), // ubs:ignore — a table case's own failure
            }
        }
    }

    #[test]
    fn every_commands_row_resolves_to_itself() {
        for row in COMMANDS {
            let separate: Vec<String> = row.name.split(' ').map(str::to_owned).collect();
            for input in [separate, vec![row.name.to_owned()]] {
                let got = resolve(&input).unwrap();
                assert_eq!(got.map(|r| r.name), Some(row.name), "{input:?}");
            }
        }
    }

    #[test]
    fn unknown_path_fixes_parse() {
        for input in [
            &["instal"][..],
            &["backup", "restor"],
            &["install", "extra"],
            &["nosuch"],
            &["auth"],
        ] {
            let Err(error) = resolve(&words(input)) else {
                continue;
            };
            let lines = std::iter::once(&error.fix).chain(error.alternatives.iter());
            for suggestion in lines {
                let argv = suggestion.argv().unwrap();
                assert!(
                    Cli::try_parse_from(argv).is_ok(),
                    "{input:?}: {argv:?} parses"
                );
                if argv[1..3] == ["robot-docs", "schema"] {
                    let path = argv[3..].to_vec();
                    assert!(
                        resolve(&path).unwrap().is_some(),
                        "{input:?}: {argv:?} names a row"
                    );
                }
            }
        }
    }

    #[test]
    fn one_path_gives_one_entry_and_nulls() {
        let one = schema(&words(&["install"])).unwrap().data;
        assert_eq!(one.commands.keys().collect::<Vec<_>>(), ["install"]);
        assert!(one.envelope.is_none() && one.events.is_none() && one.files.is_none());
        let all = schema(&[]).unwrap().data;
        assert_eq!(one.commands["install"], all.commands["install"]);
    }

    #[test]
    fn errors_data_lists_every_code_in_registry_order() {
        let data = errors().unwrap().data;
        let codes: Vec<ErrorCode> = data.error_codes.iter().map(|d| d.code).collect();
        assert_eq!(codes, ErrorCode::ALL);
        let warnings: Vec<WarningCode> = data.warning_codes.iter().map(|d| d.code).collect();
        assert_eq!(warnings, WarningCode::ALL);
    }

    #[test]
    fn errors_rows_equal_their_accessors() {
        let data = errors().unwrap().data;
        for doc in &data.error_codes {
            let code = doc.code;
            assert_eq!(doc.exit, code.exit().code(), "{code}: exit");
            assert_eq!(doc.retryable, code.retryability(), "{code}: retryable");
            assert_eq!(doc.meaning, code.meaning(), "{code}: meaning");
            assert_eq!(
                serde_json::to_value(&doc.details_schema).unwrap(),
                code.details_schema(),
                "{code}: details_schema"
            );
            assert_eq!(doc.fix_pattern, code.fix_pattern(), "{code}: fix_pattern");
        }
        for doc in &data.warning_codes {
            assert_eq!(doc.meaning, doc.code.meaning(), "{}: meaning", doc.code);
            assert_eq!(
                doc.fix_pattern,
                doc.code.fix_pattern(),
                "{}: fix_pattern",
                doc.code
            );
        }
    }

    #[test]
    fn doctor_exit_follows_the_doctor_dictionary() {
        let data = serde_json::to_value(errors().unwrap().data).unwrap();
        let rows = data["error_codes"].as_array().unwrap();
        assert_eq!(rows.len(), ErrorCode::ALL.len());
        for (row, code) in rows.iter().zip(ErrorCode::ALL) {
            assert_eq!(
                row["doctor_exit"],
                crate::doctor::exit::exit_for(*code).code(),
                "{row}"
            );
        }
        assert_eq!(
            data["error_codes"][0]
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect::<BTreeSet<_>>(),
            [
                "code",
                "details_schema",
                "doctor_exit",
                "exit",
                "fix_pattern",
                "meaning",
                "retryable"
            ]
            .map(str::to_owned)
            .into(),
        );
    }

    #[test]
    fn schema_human_form_is_the_data_pretty() {
        let outcome = schema(&words(&["config", "path"])).unwrap();
        let text = rendered(&outcome, &Style::default());
        assert!(text.ends_with("}\n") && !text.ends_with("\n\n"), "{text}");
        assert!(text.contains("\n  \"commands\""), "indented: {text}");
        assert_eq!(
            serde_json::from_str::<Value>(&text).unwrap(),
            serde_json::to_value(&outcome.data).unwrap()
        );
    }

    #[test]
    fn errors_human_form_is_a_table() {
        let outcome = errors().unwrap();
        let plain = rendered(&outcome, &Style::default());
        let verbose = rendered(
            &outcome,
            &Style {
                verbose: 1,
                ..Style::default()
            },
        );
        assert!(
            plain.starts_with(&format!("Error codes ({})\n", ErrorCode::ALL.len())),
            "{plain}"
        );
        assert!(
            plain.contains(&format!("\nWarning codes ({})\n", WarningCode::ALL.len())),
            "{plain}"
        );
        assert!(!plain.contains('\u{1b}') && !verbose.contains('\u{1b}'));
        let codes = ErrorCode::ALL.len() + WarningCode::ALL.len();
        assert_eq!(
            plain.lines().count(),
            codes + 5,
            "two headings, two header rows, a blank line"
        );
        let fix_lines: Vec<&str> = verbose
            .lines()
            .filter(|l| l.starts_with("      fix: "))
            .collect();
        assert_eq!(fix_lines.len(), codes, "one fix line per code");
        let without_fixes: Vec<&str> = verbose
            .lines()
            .filter(|l| !l.starts_with("      fix: "))
            .collect();
        assert_eq!(
            without_fixes,
            plain.lines().collect::<Vec<_>>(),
            "fix lines never widen a column"
        );
        let mut lines = verbose.lines();
        while let Some(line) = lines.next() {
            if line.starts_with("network ") {
                assert_eq!(
                    lines.next(),
                    Some(format!("      fix: {}", ErrorCode::Network.fix_pattern()).as_str())
                );
                assert!(line.contains("conditional"), "{line}");
            }
        }
    }

    #[test]
    fn outputs_are_deterministic_in_process() {
        let twice = |f: &dyn Fn() -> Vec<u8>| assert_eq!(f(), f());
        twice(&|| serde_json::to_vec(&schema(&[]).unwrap().data).unwrap());
        twice(&|| serde_json::to_vec(&errors().unwrap().data).unwrap());
        twice(&|| serde_json::to_vec(&guide().unwrap().data).unwrap());
    }

    #[test]
    fn guide_human_output_is_the_content_raw() {
        let outcome = guide().unwrap();
        let mut out = Vec::new();
        outcome.render_human(&mut out, &Style::default()).unwrap();
        assert!(
            out == outcome.data.content.as_bytes(),
            "the human form is the content, byte for byte: {} vs {} bytes",
            out.len(),
            outcome.data.content.len()
        );
        assert!(outcome.data.content.starts_with("---\nname: omaddons\n"));
    }

    #[test]
    fn guide_data_serializes_format_then_content() {
        let data = GuideData {
            format: DocFormat::Markdown,
            content: "# x\n".to_owned(),
        };
        let got = serde_json::to_string(&data).unwrap();
        assert_eq!(got, r##"{"format":"markdown","content":"# x\n"}"##);
    }

    #[test]
    fn bare_robot_docs_and_guide_share_the_canonical_path() {
        for argv in [&["robot-docs"][..], &["robot-docs", "guide"]] {
            let parsed =
                Cli::try_parse_from(std::iter::once("omaddons").chain(argv.iter().copied()));
            let got = parsed.map(|cli| super::super::command_name(cli.command.as_ref()));
            assert_eq!(got.ok(), Some(GUIDE_COMMAND), "{argv:?}");
        }
    }
}
