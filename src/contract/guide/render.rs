//! The guide's renderer (M4-D7): `{{name}}` placeholders in a hand-written template,
//! replaced in one pass by blocks generated from the registries, at fixed widths so the
//! output is byte-stable. No template engine: twelve placeholders need none.
//!
//! The grammar: a placeholder is `{{name}}` with `name` matching `[a-z][a-z0-9_]*`; a line
//! that is exactly one placeholder is a block position, anything else inline; there is no
//! escape, so a template holds no literal `{{`; text a generator returns is never scanned
//! again; a carriage return or a tab in the template is an error.

use crate::app::COMMANDS;
use crate::build_info::BUILD_ID;
use crate::contract::actions::Action;
use crate::contract::{commands, env, target_id_forms};
use crate::error::{ErrorCode, Exit, Retryability, WarningCode};
use crate::rules::{Family, RulesTable};

use super::stamp;

/// Everything the blocks render, read from the registries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuideInputs {
    /// `app::COMMANDS` order.
    pub commands: Vec<CommandLine>,
    /// `Action::ALL` order.
    pub actions: Vec<ActionRow>,
    /// `Exit::ALL`.
    pub exit_general: Vec<ExitRow>,
    /// The doctor's exit dictionary, once the doctor exists.
    pub exit_doctor: Option<Vec<ExitRow>>,
    /// [`ERRORS_COMMON`] order.
    pub errors_common: Vec<ErrorRow>,
    /// `contract::env::release()`, registry order.
    pub env: Vec<EnvRow>,
    /// `contract::target_id_forms()` order.
    pub targets: Vec<TargetFormRow>,
    /// [`NEVER`].
    pub never: Vec<&'static str>,
    pub command_count: usize,
    pub error_code_count: usize,
    pub warning_code_count: usize,
    /// Where the inputs came from. No block reads it: the guide's identity is its content
    /// (M4-D7). Present so a test can vary it and prove that.
    pub provenance: Provenance,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Provenance {
    pub tool_version: String,
    pub build_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandLine {
    pub path: String,
    pub about: String,
    pub mutates: bool,
    pub stores_plan: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionRow {
    pub action: String,
    pub description: String,
    pub destructive: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExitRow {
    pub code: u8,
    pub name: String,
    pub meaning: String,
    pub retryable: Retry,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorRow {
    pub code: String,
    pub exit: u8,
    pub exit_name: String,
    pub retryable: Retry,
    pub meaning: String,
    pub fix_pattern: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvRow {
    pub name: String,
    pub description: String,
    pub mirrors: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetFormRow {
    pub form: String,
    /// Literal values, each rendered in backticks.
    pub examples: Vec<String>,
    pub selects: String,
}

/// Whether retrying can help.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Retry {
    Yes,
    No,
    /// Each instance says (`failed`'s `error.retryable`; a conditional code).
    Varies,
    /// Success.
    NotApplicable,
}

impl GuideInputs {
    /// The inputs of this build. It reads no file: no `config.toml`, no client, no
    /// network, and the built-in rules, never the effective ones, so the guide is the same
    /// bytes on every machine.
    pub fn live() -> Self {
        let rules = RulesTable::builtin();
        Self {
            commands: commands::commands()
                .into_iter()
                .map(|info| CommandLine {
                    path: info.path.to_owned(),
                    about: info.about,
                    mutates: info.mutates,
                    stores_plan: info.stores_plan,
                })
                .collect(),
            actions: Action::ALL
                .iter()
                .map(|action| ActionRow {
                    action: action.wire().to_owned(),
                    description: action.description().to_owned(),
                    destructive: action.destructive(),
                })
                .collect(),
            exit_general: Exit::ALL.iter().map(|&exit| exit_row(exit)).collect(),
            exit_doctor: Some(
                crate::doctor::exit::Exit::ALL
                    .iter()
                    .map(|exit| ExitRow {
                        code: exit.code(),
                        name: exit.name().to_owned(),
                        meaning: exit.meaning().to_owned(),
                        // Not shown: the doctor's table has no retry column.
                        retryable: Retry::NotApplicable,
                    })
                    .collect(),
            ),
            errors_common: ERRORS_COMMON
                .iter()
                .map(|&code| ErrorRow {
                    code: code.wire().to_owned(),
                    exit: code.exit().code(),
                    exit_name: code.exit().name().to_owned(),
                    retryable: match code.retryability() {
                        Retryability::Fixed(true) => Retry::Yes,
                        Retryability::Fixed(false) => Retry::No,
                        Retryability::Conditional(_) => Retry::Varies,
                    },
                    meaning: code.meaning().to_owned(),
                    fix_pattern: code.fix_pattern().to_owned(),
                })
                .collect(),
            env: env::release()
                .map(|var| EnvRow {
                    name: var.name.to_owned(),
                    description: var.description.to_owned(),
                    mirrors: var.mirrors.map(str::to_owned),
                })
                .collect(),
            targets: target_id_forms()
                .into_iter()
                .map(|form| target_row(&rules, form))
                .collect(),
            never: NEVER.to_vec(),
            command_count: COMMANDS.len(),
            error_code_count: ErrorCode::ALL.len(),
            warning_code_count: WarningCode::ALL.len(),
            provenance: Provenance {
                tool_version: env!("CARGO_PKG_VERSION").to_owned(),
                build_id: Some(BUILD_ID.to_owned()),
            },
        }
    }
}

fn exit_row(exit: Exit) -> ExitRow {
    ExitRow {
        code: exit.code(),
        name: exit.name().to_owned(),
        meaning: exit.meaning().to_owned(),
        retryable: match (exit, exit.retryable()) {
            (_, Some(true)) => Retry::Yes,
            (_, Some(false)) => Retry::No,
            (Exit::Ok, None) => Retry::NotApplicable,
            (_, None) => Retry::Varies,
        },
    }
}

/// A `-t` form's row; empty cells for a form [`TARGET_FORMS`] lacks, which a unit test
/// fails.
fn target_row(rules: &RulesTable, form: &str) -> TargetFormRow {
    let row = TARGET_FORMS.iter().find(|row| row.form == form);
    TargetFormRow {
        form: form.to_owned(),
        examples: row.map(|row| (row.examples)(rules)).unwrap_or_default(),
        selects: row.map(|row| row.selects.to_owned()).unwrap_or_default(),
    }
}

/// One `-t` form as the guide explains it: examples computed from the built-in rules, and
/// what it selects.
pub struct TargetForm {
    pub form: &'static str,
    pub examples: fn(&RulesTable) -> Vec<String>,
    pub selects: &'static str,
}

pub const TARGET_FORMS: &[TargetForm] = &[
    TargetForm {
        form: "key",
        examples: |_| vec!["<key>".to_owned()],
        selects: "one client by its stable key, as `omaddons targets --json` lists it",
    },
    TargetForm {
        form: "id",
        examples: id_examples,
        selects: "one client by its id",
    },
    TargetForm {
        form: "product_code",
        examples: |rules| {
            product(rules, "wow_classic_era", |p| p.code.clone())
                .into_iter()
                .collect()
        },
        selects: "the client of that Battle.net product",
    },
    TargetForm {
        form: "flavor_folder",
        examples: |rules| {
            product(rules, "wow", |p| p.folder.clone())
                .into_iter()
                .collect()
        },
        selects: "the client in that flavor folder",
    },
    TargetForm {
        form: "game_mode",
        examples: |rules| {
            rules
                .products_with_mode("sod")
                .into_iter()
                .filter(|(p, _)| p.code == "wow_classic_era")
                .map(|(_, mode)| mode.id.clone())
                .take(1)
                .collect()
        },
        selects: "the client that runs the mode (`sod` selects Classic Era)",
    },
    TargetForm {
        form: "family",
        examples: |_| vec![Family::Mainline.as_str().to_owned()],
        selects: "every client of the family",
    },
    TargetForm {
        form: "path",
        examples: |_| vec!["<client dir>".to_owned()],
        selects: "the client at that install root, client directory or `AddOns` path",
    },
    TargetForm {
        form: "all",
        examples: |_| vec!["all".to_owned()],
        selects: "every client",
    },
];

fn product(
    rules: &RulesTable,
    code: &str,
    field: fn(&crate::rules::Product) -> String,
) -> Option<String> {
    rules.product_by_code(code).map(field)
}

/// `retail`, `era`, `wotlk`: two products' default ids and a custom family's.
fn id_examples(rules: &RulesTable) -> Vec<String> {
    [
        product(rules, "wow", |p| p.default_id.clone()),
        product(rules, "wow_classic_era", |p| p.default_id.clone()),
        rules
            .default_id_for_family(Family::WrathLegacy)
            .map(str::to_owned),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// The codes the core loop meets: a wrong invocation, an unresolved client or ref, a
/// refused install, a running game, a contended lock, a stale preview, the network.
pub const ERRORS_COMMON: [ErrorCode; 15] = [
    ErrorCode::Usage,
    ErrorCode::TargetNotFound,
    ErrorCode::TargetAmbiguous,
    ErrorCode::RefAmbiguous,
    ErrorCode::NotFound,
    ErrorCode::NotManaged,
    ErrorCode::NoCompatibleRelease,
    ErrorCode::WrongFlavor,
    ErrorCode::GameRunning,
    ErrorCode::UnmanagedFolder,
    ErrorCode::UndoConflict,
    ErrorCode::Locked,
    ErrorCode::PlanStale,
    ErrorCode::Network,
    ErrorCode::RateLimited,
];

/// Things omaddons will never do: the guide's list, and `doctor robot-docs`'s.
pub const NEVER: &[&str] = &[
    "Delete your files during a change. Removals go into undo snapshots or a quarantine, and only `backup prune`, `doctor gc --before --yes` and retention delete, by their documented rules.",
    "Write into `Interface/AddOns` outside a transaction, touch a `Blizzard_*` folder, or follow a symlinked folder.",
    "Write SavedVariables while the client runs, or parse or edit them, except reading BugGrabber's.",
    "Rewrite `history.jsonl`, `config.toml` or `addons.toml` behind your back (`config set` and `export --write` edit them only when asked).",
    "Contact a host outside the allowlist, make a request under `--offline`, or send a credential anywhere but its own service.",
    "Run a shell, or interpret a string as a command: argv only.",
    "Prompt for input, or print ANSI under `--json`.",
    "Act on state it cannot read (a newer or damaged journal, record or history line): it refuses and says why.",
];

/// The other coding agents' skill roots, relative to `$HOME` (M4-D8): `robot-docs
/// install-skill` offers one `--dir <root>/omaddons` hint for each that exists, and never
/// writes there unasked. Claude Code's (`.claude/skills`) is the default.
pub const OTHER_SKILL_ROOTS: &[&str] = &[".codex/skills", ".agents/skills"];

/// Where a placeholder may stand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Alone on its line.
    Block,
    /// Anywhere.
    Inline,
}

pub struct Placeholder {
    pub name: &'static str,
    pub kind: Kind,
    pub render: fn(&GuideInputs) -> String,
}

/// The stamp's name: filled last, by [`render`] itself.
const STAMP: &str = "stamp";

pub const PLACEHOLDERS: &[Placeholder] = &[
    Placeholder {
        name: STAMP,
        kind: Kind::Block,
        render: |_| stamp::line(&"0".repeat(64)),
    },
    Placeholder {
        name: "commands",
        kind: Kind::Block,
        render: commands_block,
    },
    Placeholder {
        name: "actions",
        kind: Kind::Block,
        render: actions_block,
    },
    Placeholder {
        name: "exit_general",
        kind: Kind::Block,
        render: |inputs| exit_table(&inputs.exit_general, true),
    },
    Placeholder {
        name: "exit_doctor",
        kind: Kind::Block,
        render: exit_doctor_block,
    },
    Placeholder {
        name: "errors_common",
        kind: Kind::Block,
        render: errors_common_block,
    },
    Placeholder {
        name: "never",
        kind: Kind::Block,
        render: never_block,
    },
    Placeholder {
        name: "env",
        kind: Kind::Block,
        render: env_block,
    },
    Placeholder {
        name: "targets",
        kind: Kind::Block,
        render: targets_block,
    },
    Placeholder {
        name: "command_count",
        kind: Kind::Inline,
        render: |inputs| inputs.command_count.to_string(),
    },
    Placeholder {
        name: "error_code_count",
        kind: Kind::Inline,
        render: |inputs| inputs.error_code_count.to_string(),
    },
    Placeholder {
        name: "warning_code_count",
        kind: Kind::Inline,
        render: |inputs| inputs.warning_code_count.to_string(),
    },
];

/// Why a template did not render. Each names its 1-based template line.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RenderError {
    #[error("template line {line}: unknown placeholder {{{{{name}}}}}")]
    UnknownPlaceholder { name: String, line: usize },
    #[error(
        "template line {line}: {text} is not a placeholder: write {{{{name}}}} with a lowercase name"
    )]
    BadPlaceholder { text: String, line: usize },
    #[error("template line {line}: a {{{{ with no }}}} after it on the line")]
    Unterminated { line: usize },
    #[error("template line {line}: {{{{{name}}}}} is a block and must stand alone on its line")]
    BlockInline { name: String, line: usize },
    #[error("template line {line}: {{{{stamp}}}} must be the line right after the frontmatter")]
    StampMisplaced { line: usize },
    #[error("template line {line}: a second {{{{stamp}}}}")]
    DuplicateStamp { line: usize },
    #[error("template line {line}: a {what}")]
    ForbiddenCharacter { what: &'static str, line: usize },
}

/// One piece of a template line.
enum Piece<'a> {
    Text(&'a str),
    Placeholder(&'a str),
}

/// `line`'s text and placeholders, left to right.
fn pieces(line: &str, number: usize) -> Result<Vec<Piece<'_>>, RenderError> {
    let mut out = Vec::new();
    let mut rest = line;
    while let Some(open) = rest.find("{{") {
        out.push(Piece::Text(&rest[..open]));
        let after = &rest[open + 2..];
        let close = after
            .find("}}")
            .ok_or(RenderError::Unterminated { line: number })?;
        let name = &after[..close];
        let valid = name.starts_with(|c: char| c.is_ascii_lowercase())
            && name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
        if !valid {
            return Err(RenderError::BadPlaceholder {
                text: format!("{{{{{name}}}}}"),
                line: number,
            });
        }
        out.push(Piece::Placeholder(name));
        rest = &after[close + 2..];
    }
    out.push(Piece::Text(rest));
    Ok(out)
}

fn placeholder(name: &str, line: usize) -> Result<&'static Placeholder, RenderError> {
    PLACEHOLDERS
        .iter()
        .find(|p| p.name == name)
        .ok_or_else(|| RenderError::UnknownPlaceholder {
            name: name.to_owned(),
            line,
        })
}

/// `template` with every placeholder replaced, LF line ends and one final newline. A
/// `{{stamp}}` on the line right after the frontmatter is filled last with the digest of
/// the rest; a template without one renders with no stamp.
pub fn render(template: &str, inputs: &GuideInputs) -> Result<String, RenderError> {
    let mut lines: Vec<&str> = template.split('\n').collect();
    if template.ends_with('\n') {
        lines.pop();
    }
    for (i, line) in lines.iter().enumerate() {
        for (c, what) in [('\r', "carriage return"), ('\t', "tab")] {
            if line.contains(c) {
                return Err(RenderError::ForbiddenCharacter { what, line: i + 1 });
            }
        }
    }
    let closing_fence = (lines.first() == Some(&"---"))
        .then(|| {
            lines
                .iter()
                .skip(1)
                .position(|line| *line == "---")
                .map(|i| i + 1)
        })
        .flatten();

    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut stamp_at = None;
    for (i, line) in lines.iter().enumerate() {
        let number = i + 1;
        let parts = pieces(line, number)?;
        if let [Piece::Text(""), Piece::Placeholder(name), Piece::Text("")] = parts.as_slice() {
            let found = placeholder(name, number)?;
            if found.name == STAMP {
                if stamp_at.is_some() {
                    return Err(RenderError::DuplicateStamp { line: number });
                }
                if closing_fence.is_none_or(|fence| i != fence + 1) {
                    return Err(RenderError::StampMisplaced { line: number });
                }
                stamp_at = Some(out.len());
            }
            out.push((found.render)(inputs));
            continue;
        }
        let mut text = String::new();
        for part in parts {
            match part {
                Piece::Text(t) => text.push_str(t),
                Piece::Placeholder(name) => {
                    let found = placeholder(name, number)?;
                    if found.kind == Kind::Block {
                        return Err(RenderError::BlockInline {
                            name: name.to_owned(),
                            line: number,
                        });
                    }
                    text.push_str(&(found.render)(inputs));
                }
            }
        }
        out.push(text);
    }

    let joined = |out: &[String]| format!("{}\n", out.join("\n"));
    let Some(at) = stamp_at else {
        return Ok(joined(&out));
    };
    // The provisional line holds zeros; the digest ignores that line, so it is the digest
    // of the finished document too.
    let digest =
        stamp::digest_without_stamp(&joined(&out)).map_err(|_| RenderError::StampMisplaced {
            line: closing_fence.map_or(1, |fence| fence + 2),
        })?;
    out[at] = stamp::line(&stamp::hex(&digest));
    Ok(joined(&out))
}

/// `yes`, `no`, `see error.retryable` or `n/a`.
fn retry_word(retry: Retry) -> &'static str {
    match retry {
        Retry::Yes => "yes",
        Retry::No => "no",
        Retry::Varies => "see error.retryable",
        Retry::NotApplicable => "n/a",
    }
}

fn yes_no(yes: bool) -> String {
    if yes { "yes" } else { "no" }.to_owned()
}

fn code(text: &str) -> String {
    format!("`{text}`")
}

/// One line of text: a newline in generated text would end a list item or a table row.
fn one_line(text: &str) -> String {
    text.replace(['\r', '\n'], " ")
}

fn commands_block(inputs: &GuideInputs) -> String {
    let lines: Vec<String> = inputs
        .commands
        .iter()
        .map(|command| {
            let marks = match (command.mutates, command.stores_plan) {
                (true, true) => " (mutates · stores plan)",
                (true, false) => " (mutates)",
                (false, true) => " (stores plan)",
                (false, false) => "",
            };
            format!(
                "- {} — {}{marks}",
                code(&command.path),
                one_line(&command.about)
            )
        })
        .collect();
    lines.join("\n")
}

fn actions_block(inputs: &GuideInputs) -> String {
    let rows: Vec<Vec<String>> = inputs
        .actions
        .iter()
        .map(|row| {
            vec![
                code(&row.action),
                row.description.clone(),
                yes_no(row.destructive),
            ]
        })
        .collect();
    table(&["Action", "What it does", "Destructive"], &rows)
}

fn exit_table(rows: &[ExitRow], with_retryable: bool) -> String {
    let rows: Vec<Vec<String>> = rows
        .iter()
        .map(|row| {
            let mut cells = vec![row.code.to_string(), code(&row.name), row.meaning.clone()];
            if with_retryable {
                cells.push(retry_word(row.retryable).to_owned());
            }
            cells
        })
        .collect();
    if with_retryable {
        table(&["Code", "Name", "Meaning", "Retryable"], &rows)
    } else {
        table(&["Code", "Name", "Meaning"], &rows)
    }
}

fn exit_doctor_block(inputs: &GuideInputs) -> String {
    match &inputs.exit_doctor {
        None => "This build has no doctor, and so no doctor exit dictionary.".to_owned(),
        Some(rows) => format!(
            "Every `doctor` command uses its own exit dictionary:\n\n{}",
            exit_table(rows, false)
        ),
    }
}

fn errors_common_block(inputs: &GuideInputs) -> String {
    let mut lines: Vec<String> = inputs
        .errors_common
        .iter()
        .map(|row| {
            let retryable = match row.retryable {
                Retry::Yes => "yes",
                Retry::No => "no",
                Retry::Varies => "per instance",
                Retry::NotApplicable => "n/a",
            };
            let fix = one_line(&row.fix_pattern);
            let period = if fix.ends_with('.') { "" } else { "." };
            format!(
                "- {} (exit {} {}; retryable: {retryable}). {} Fix: {fix}{period}",
                code(&row.code),
                row.exit,
                code(&row.exit_name),
                one_line(&row.meaning),
            )
        })
        .collect();
    lines.push(String::new());
    lines.push("The rest: `omaddons robot-docs errors`.".to_owned());
    lines.join("\n")
}

fn never_block(inputs: &GuideInputs) -> String {
    let lines: Vec<String> = inputs
        .never
        .iter()
        .enumerate()
        .map(|(i, item)| format!("{}. {}", i + 1, one_line(item)))
        .collect();
    lines.join("\n")
}

fn env_block(inputs: &GuideInputs) -> String {
    let rows: Vec<Vec<String>> = inputs
        .env
        .iter()
        .map(|row| {
            vec![
                code(&row.name),
                row.description.clone(),
                row.mirrors.as_deref().map(code).unwrap_or_default(),
            ]
        })
        .collect();
    table(&["Variable", "Effect", "Mirrors"], &rows)
}

fn targets_block(inputs: &GuideInputs) -> String {
    let rows: Vec<Vec<String>> = inputs
        .targets
        .iter()
        .map(|row| {
            let examples: Vec<String> = row.examples.iter().map(|e| code(e)).collect();
            vec![code(&row.form), examples.join(", "), row.selects.clone()]
        })
        .collect();
    table(&["Form", "Example", "Selects"], &rows)
}

/// A GitHub-flavoured pipe table: each column padded to its widest cell in `char`s (never
/// a terminal's width), the separator padded with dashes, `|` escaped as `\|`, newlines
/// replaced by spaces, no trailing whitespace.
fn table(header: &[&str], rows: &[Vec<String>]) -> String {
    let clean = |cell: &str| one_line(cell).replace('|', "\\|");
    let mut cells: Vec<Vec<String>> = vec![header.iter().map(|h| clean(h)).collect()];
    cells.extend(
        rows.iter()
            .map(|row| row.iter().map(|c| clean(c)).collect()),
    );
    let mut widths = vec![0; header.len()];
    for row in &cells {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }
    let line = |row: &[String]| {
        let padded: Vec<String> = widths
            .iter()
            .enumerate()
            .map(|(i, width)| {
                let cell = row.get(i).map_or("", String::as_str);
                let pad = width.saturating_sub(cell.chars().count());
                format!("{cell}{}", " ".repeat(pad))
            })
            .collect();
        format!("| {} |", padded.join(" | "))
    };
    let separator: Vec<String> = widths.iter().map(|width| "-".repeat(*width)).collect();
    let mut out = vec![line(&cells[0]), format!("| {} |", separator.join(" | "))];
    out.extend(cells[1..].iter().map(|row| line(row)));
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    fn sample() -> GuideInputs {
        GuideInputs {
            commands: vec![
                CommandLine {
                    path: "status".into(),
                    about: "Show every client".into(),
                    mutates: false,
                    stores_plan: false,
                },
                CommandLine {
                    path: "remove".into(),
                    about: "Remove addons".into(),
                    mutates: true,
                    stores_plan: false,
                },
                CommandLine {
                    path: "install".into(),
                    about: "Install addons".into(),
                    mutates: true,
                    stores_plan: true,
                },
            ],
            actions: vec![
                ActionRow {
                    action: "apply".into(),
                    description: "Run the change".into(),
                    destructive: false,
                },
                ActionRow {
                    action: "remove".into(),
                    description: "Remove it".into(),
                    destructive: true,
                },
            ],
            exit_general: Exit::ALL.iter().map(|&exit| exit_row(exit)).collect(),
            exit_doctor: None,
            errors_common: vec![
                ErrorRow {
                    code: "usage".into(),
                    exit: 2,
                    exit_name: "usage".into(),
                    retryable: Retry::No,
                    meaning: "Bad command line.".into(),
                    fix_pattern: "the corrected command line".into(),
                },
                ErrorRow {
                    code: "network".into(),
                    exit: 1,
                    exit_name: "failed".into(),
                    retryable: Retry::Varies,
                    meaning: "A connection failed.".into(),
                    fix_pattern: "the same command again.".into(),
                },
            ],
            env: vec![
                EnvRow {
                    name: "OMADDONS_OFFLINE".into(),
                    description: "1 means --offline".into(),
                    mirrors: Some("--offline".into()),
                },
                EnvRow {
                    name: "NO_COLOR".into(),
                    description: "no colour".into(),
                    mirrors: None,
                },
            ],
            targets: vec![
                TargetFormRow {
                    form: "id".into(),
                    examples: vec!["retail".into(), "era".into()],
                    selects: "one client by its id".into(),
                },
                TargetFormRow {
                    form: "path".into(),
                    examples: vec!["<client dir>".into()],
                    selects: "the client there".into(),
                },
            ],
            never: vec!["Delete your files.", "Run a shell."],
            command_count: 3,
            error_code_count: 41,
            warning_code_count: 54,
            provenance: Provenance {
                tool_version: "0.1.0".into(),
                build_id: None,
            },
        }
    }

    fn rendered(template: &str) -> Result<String, RenderError> {
        render(template, &sample())
    }

    fn block(name: &str, inputs: &GuideInputs) -> String {
        let found = PLACEHOLDERS.iter().find(|p| p.name == name).unwrap();
        (found.render)(inputs)
    }

    fn assert_text(case: &str, got: &str, want: &str) {
        assert!(got == want, "{case}:\n--- got\n{got}\n--- expected\n{want}");
    }

    #[test]
    fn placeholder_cases() {
        let cases: Vec<(&str, &str, Result<String, RenderError>)> = vec![
            (
                "block_alone",
                "a\n{{never}}\nb\n",
                Ok("a\n1. Delete your files.\n2. Run a shell.\nb\n".into()),
            ),
            (
                "inline_in_text",
                "has {{command_count}} commands\n",
                Ok("has 3 commands\n".into()),
            ),
            (
                "two_inlines_on_one_line",
                "{{error_code_count}}/{{warning_code_count}}\n",
                Ok("41/54\n".into()),
            ),
            (
                "inline_alone_on_a_line",
                "{{command_count}}\n",
                Ok("3\n".into()),
            ),
            (
                "block_used_inline",
                "see {{never}}\n",
                Err(RenderError::BlockInline {
                    name: "never".into(),
                    line: 1,
                }),
            ),
            (
                "unknown_block",
                "a\nb\n{{nope}}\n",
                Err(RenderError::UnknownPlaceholder {
                    name: "nope".into(),
                    line: 3,
                }),
            ),
            (
                "unknown_inline",
                "a\nb\nx {{nope}} y\n",
                Err(RenderError::UnknownPlaceholder {
                    name: "nope".into(),
                    line: 3,
                }),
            ),
            (
                "unterminated",
                "a {{never\n",
                Err(RenderError::Unterminated { line: 1 }),
            ),
            (
                "spaced_name",
                "{{ commands }}\n",
                Err(RenderError::BadPlaceholder {
                    text: "{{ commands }}".into(),
                    line: 1,
                }),
            ),
            (
                "uppercase_name",
                "{{Commands}}\n",
                Err(RenderError::BadPlaceholder {
                    text: "{{Commands}}".into(),
                    line: 1,
                }),
            ),
            (
                "empty_name",
                "{{}}\n",
                Err(RenderError::BadPlaceholder {
                    text: "{{}}".into(),
                    line: 1,
                }),
            ),
            (
                "triple_brace",
                "{{{never}}}\n",
                Err(RenderError::BadPlaceholder {
                    text: "{{{never}}".into(),
                    line: 1,
                }),
            ),
            ("lone_closing_braces", "a }} b\n", Ok("a }} b\n".into())),
            (
                "carriage_return",
                "a\r\nb\n",
                Err(RenderError::ForbiddenCharacter {
                    what: "carriage return",
                    line: 1,
                }),
            ),
            (
                "tab",
                "a\n\tb\n",
                Err(RenderError::ForbiddenCharacter {
                    what: "tab",
                    line: 2,
                }),
            ),
            ("no_trailing_newline", "a\nb", Ok("a\nb\n".into())),
            ("blank_last_line_kept", "a\n\n", Ok("a\n\n".into())),
        ];
        for (case, input, want) in cases {
            let got = rendered(input);
            assert_eq!(
                got, want,
                "{case}: input {input:?} gave {got:?}, expected {want:?}"
            );
        }
        let message = rendered("a\nb\n{{nope}}\n").unwrap_err().to_string();
        assert_eq!(message, "template line 3: unknown placeholder {{nope}}");
    }

    #[test]
    fn generated_text_is_not_rescanned() {
        let mut inputs = sample();
        inputs.commands[0].about = "uses {{never}}".into();
        let got = render("{{commands}}\n", &inputs).unwrap();
        assert!(got.contains("`status` — uses {{never}}\n"), "{got}");
    }

    #[test]
    fn stamp_cases() {
        let stamped = rendered("---\nname: x\n---\n{{stamp}}\nbody {{command_count}}\n").unwrap();
        assert_eq!(
            stamp::verifies(&stamped),
            Ok(true),
            "after_frontmatter: {stamped}"
        );
        assert!(stamped.ends_with("\nbody 3\n"), "{stamped}");
        let cases: Vec<(&str, &str, RenderError)> = vec![
            (
                "blank_line_before_stamp",
                "---\nname: x\n---\n\n{{stamp}}\n",
                RenderError::StampMisplaced { line: 5 },
            ),
            (
                "without_frontmatter",
                "{{stamp}}\nbody\n",
                RenderError::StampMisplaced { line: 1 },
            ),
            (
                "inside_frontmatter",
                "---\n{{stamp}}\n---\n",
                RenderError::StampMisplaced { line: 2 },
            ),
            (
                "two_stamps",
                "---\nname: x\n---\n{{stamp}}\n{{stamp}}\n",
                RenderError::DuplicateStamp { line: 5 },
            ),
            (
                "inline_stamp",
                "---\nname: x\n---\nstamp: {{stamp}}\n",
                RenderError::BlockInline {
                    name: "stamp".into(),
                    line: 4,
                },
            ),
        ];
        for (case, input, want) in cases {
            let got = rendered(input);
            assert_eq!(
                got,
                Err(want.clone()),
                "{case}: input {input:?} gave {got:?}, expected {want:?}"
            );
        }
        let plain = rendered("---\nname: x\n---\nbody\n").unwrap();
        assert_eq!(plain, "---\nname: x\n---\nbody\n", "no_stamp_in_template");
    }

    #[test]
    fn commands_block_lines() {
        assert_text(
            "commands",
            &block("commands", &sample()),
            "- `status` — Show every client\n- `remove` — Remove addons (mutates)\n- `install` — Install addons (mutates · stores plan)",
        );
    }

    #[test]
    fn actions_block_table() {
        assert_text(
            "actions",
            &block("actions", &sample()),
            "| Action   | What it does   | Destructive |\n| -------- | -------------- | ----------- |\n| `apply`  | Run the change | no          |\n| `remove` | Remove it      | yes         |",
        );
    }

    #[test]
    fn exit_general_block_table() {
        let got = block("exit_general", &sample());
        let lines: Vec<&str> = got.lines().collect();
        assert_eq!(lines.len(), 2 + Exit::ALL.len(), "{got}");
        assert!(
            lines[0].starts_with("| Code | Name ")
                && lines[0].ends_with(" | Retryable           |"),
            "{got}"
        );
        assert!(lines[1].starts_with("| ---- | ---"), "{got}");
        let row = |name: &str| {
            lines
                .iter()
                .find(|line| line.contains(&format!("`{name}`")))
                .copied()
                .unwrap_or("")
        };
        assert!(
            row("ok").starts_with("| 0    | `ok`")
                && row("ok").ends_with("| n/a                 |"),
            "{got}"
        );
        assert!(row("failed").ends_with("| see error.retryable |"), "{got}");
        assert!(row("locked").ends_with("| yes                 |"), "{got}");
        assert!(row("usage").ends_with("| no                  |"), "{got}");
    }

    #[test]
    fn exit_doctor_block_holding_line() {
        assert_text(
            "holding",
            &block("exit_doctor", &sample()),
            "This build has no doctor, and so no doctor exit dictionary.",
        );
    }

    #[test]
    fn exit_doctor_block_table() {
        let mut inputs = sample();
        inputs.exit_doctor = Some(vec![
            ExitRow {
                code: 0,
                name: "healthy".into(),
                meaning: "Nothing to fix".into(),
                retryable: Retry::NotApplicable,
            },
            ExitRow {
                code: 10,
                name: "findings".into(),
                meaning: "Something to fix".into(),
                retryable: Retry::No,
            },
        ]);
        assert_text(
            "doctor",
            &block("exit_doctor", &inputs),
            "Every `doctor` command uses its own exit dictionary:\n\n| Code | Name       | Meaning          |\n| ---- | ---------- | ---------------- |\n| 0    | `healthy`  | Nothing to fix   |\n| 10   | `findings` | Something to fix |",
        );
    }

    #[test]
    fn errors_common_block_lines_and_pointer() {
        assert_text(
            "errors",
            &block("errors_common", &sample()),
            "- `usage` (exit 2 `usage`; retryable: no). Bad command line. Fix: the corrected command line.\n- `network` (exit 1 `failed`; retryable: per instance). A connection failed. Fix: the same command again.\n\nThe rest: `omaddons robot-docs errors`.",
        );
    }

    #[test]
    fn never_block_numbered() {
        assert_text(
            "never",
            &block("never", &sample()),
            "1. Delete your files.\n2. Run a shell.",
        );
    }

    #[test]
    fn env_block_table() {
        assert_text(
            "env",
            &block("env", &sample()),
            "| Variable           | Effect            | Mirrors     |\n| ------------------ | ----------------- | ----------- |\n| `OMADDONS_OFFLINE` | 1 means --offline | `--offline` |\n| `NO_COLOR`         | no colour         |             |",
        );
    }

    #[test]
    fn targets_block_table() {
        assert_text(
            "targets",
            &block("targets", &sample()),
            "| Form   | Example         | Selects              |\n| ------ | --------------- | -------------------- |\n| `id`   | `retail`, `era` | one client by its id |\n| `path` | `<client dir>`  | the client there     |",
        );
    }

    #[test]
    fn counts_render_in_decimal() {
        let got = rendered("{{command_count}} {{error_code_count}} {{warning_code_count}}\n");
        assert_eq!(got, Ok("3 41 54\n".to_owned()));
    }

    #[test]
    fn table_pads_by_characters_not_bytes() {
        let rows = vec![
            vec!["a — b".to_owned(), "x".to_owned()],
            vec!["abcdefg".to_owned(), "y".to_owned()],
        ];
        assert_text(
            "dash",
            &table(&["Head", "H"], &rows),
            "| Head    | H |\n| ------- | - |\n| a — b   | x |\n| abcdefg | y |",
        );
    }

    #[test]
    fn table_escapes_pipes_and_newlines() {
        let rows = vec![vec!["a|b".to_owned(), "two\nlines".to_owned()]];
        assert_text(
            "escape",
            &table(&["Cell", "Text"], &rows),
            "| Cell | Text      |\n| ---- | --------- |\n| a\\|b | two lines |",
        );
    }

    #[test]
    fn live_inputs_cover_the_registries() {
        let live = GuideInputs::live();
        let paths: Vec<&str> = live.commands.iter().map(|c| c.path.as_str()).collect();
        let rows: Vec<&str> = COMMANDS.iter().map(|row| row.name).collect();
        assert_eq!(paths, rows, "every COMMANDS path, in order");
        for command in &live.commands {
            assert!(!command.about.is_empty(), "{}: an about", command.path);
        }
        let actions: Vec<&str> = live.actions.iter().map(|a| a.action.as_str()).collect();
        assert_eq!(
            actions,
            Action::ALL.iter().map(|a| a.wire()).collect::<Vec<_>>(),
            "Action::ALL"
        );
        let exits: Vec<u8> = live.exit_general.iter().map(|e| e.code).collect();
        assert_eq!(
            exits,
            Exit::ALL.iter().map(|e| e.code()).collect::<Vec<_>>(),
            "Exit::ALL"
        );
        let env: Vec<&str> = live.env.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            env,
            env::release().map(|v| v.name).collect::<Vec<_>>(),
            "the release variables"
        );
        let forms: Vec<&str> = live.targets.iter().map(|t| t.form.as_str()).collect();
        assert_eq!(forms, target_id_forms(), "one row per -t form");
        assert_eq!(
            (
                live.command_count,
                live.error_code_count,
                live.warning_code_count
            ),
            (COMMANDS.len(), ErrorCode::ALL.len(), WarningCode::ALL.len()),
            "the counts"
        );
        assert_eq!(live.never, NEVER, "NEVER");
        let doctor: Vec<(u8, String)> = live
            .exit_doctor
            .unwrap_or_default()
            .into_iter()
            .map(|row| (row.code, row.name))
            .collect();
        let dictionary: Vec<(u8, String)> = crate::doctor::exit::Exit::ALL
            .iter()
            .map(|exit| (exit.code(), exit.name().to_owned()))
            .collect();
        assert_eq!(doctor, dictionary, "the doctor's dictionary, in code order");
    }

    #[test]
    fn errors_common_is_fifteen_distinct_registered_codes() {
        let distinct: BTreeSet<&str> = ERRORS_COMMON.iter().map(|c| c.wire()).collect();
        assert_eq!(distinct.len(), 15, "fifteen distinct codes");
        assert!(
            ERRORS_COMMON.iter().all(|c| ErrorCode::ALL.contains(c)),
            "registered"
        );
    }

    #[test]
    fn every_target_form_has_an_example() {
        let rules = RulesTable::builtin();
        for form in target_id_forms() {
            let row = target_row(&rules, form);
            assert!(
                !row.examples.is_empty() && !row.selects.is_empty(),
                "add an example and a description for target form `{form}` in TARGET_FORMS"
            );
        }
    }

    #[test]
    fn target_examples_come_from_the_builtin_rules() {
        let rules = RulesTable::builtin();
        let examples = |form: &str| target_row(&rules, form).examples;
        let wow = rules.product_by_code("wow").unwrap();
        let era = rules.product_by_code("wow_classic_era").unwrap();
        assert_eq!(
            examples("id"),
            [
                wow.default_id.clone(),
                era.default_id.clone(),
                rules
                    .default_id_for_family(Family::WrathLegacy)
                    .unwrap()
                    .to_owned()
            ],
        );
        assert_eq!(
            examples("id"),
            ["retail", "era", "wotlk"],
            "the documented ids"
        );
        assert_eq!(examples("product_code"), std::slice::from_ref(&era.code));
        assert_eq!(examples("flavor_folder"), std::slice::from_ref(&wow.folder));
        assert_eq!(examples("flavor_folder"), ["_retail_"]);
        let modes = rules.products_with_mode("sod");
        assert!(
            modes
                .iter()
                .any(|(p, m)| p.code == "wow_classic_era" && m.id == "sod"),
            "sod is a Classic Era mode"
        );
        assert_eq!(examples("game_mode"), ["sod"]);
        assert_eq!(examples("family"), [Family::Mainline.as_str()]);
    }

    #[test]
    fn never_has_eight_sentences() {
        assert_eq!(NEVER.len(), 8);
        for item in NEVER {
            assert!(
                !item.is_empty() && !item.contains('\n') && item.ends_with('.'),
                "{item:?}"
            );
        }
    }

    #[test]
    fn provenance_never_reaches_the_output() {
        let mut outputs = BTreeSet::new();
        for (version, build_id) in [("0.1.0", "0000000000000000"), ("9.9.9", "ffffffffffffffff")] {
            let mut inputs = GuideInputs::live();
            inputs.provenance = Provenance {
                tool_version: version.into(),
                build_id: Some(build_id.into()),
            };
            outputs.insert(render(super::super::TEMPLATE, &inputs).unwrap());
        }
        assert_eq!(outputs.len(), 1, "the provenance changes no byte");
    }

    #[test]
    fn rendering_is_deterministic() {
        let render_live = || render(super::super::TEMPLATE, &GuideInputs::live());
        assert_eq!(render_live(), render_live());
    }
}
