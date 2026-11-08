//! `omaddons`, `omaddons help [<command>]`, `-h` and `-V`. Help is data: generated
//! from the live clap tree, the exit dictionary and the discovery table, so it never
//! advertises a command this build does not have (a test holds every discovery entry to
//! the clap tree and `COMMANDS`), and it is concise enough for an agent's first contact.

use std::io::Write;

use clap::CommandFactory;
use clap::builder::styling::{AnsiColor, Style as Ansi, Styles};
use schemars::JsonSchema;
use serde::Serialize;

use super::{Group, group_of};
use crate::cli::Cli;
use crate::error::{Error, ErrorCode, Exit, Suggestion};
use crate::output::{Outcome, Style, table};

/// The column the command names of a group line start at.
const GROUP_COLUMN: usize = 10;
/// Bare help's group lines and prose stay narrower than this many characters (the exit
/// table is as wide as its meanings).
const MAX_WIDTH: usize = 80;
/// The widest a command's page gets: it wraps to the terminal, never wider than this.
pub const HELP_MAX_COLUMNS: usize = 100;
/// How help looks where the output policy allows color: headings bold and underlined,
/// usage and literals (flags, commands) bold, placeholders dimmed, fixed values green.
pub const STYLES: Styles = Styles::styled()
    .header(Ansi::new().bold().underline())
    .usage(Ansi::new().bold())
    .literal(Ansi::new().bold())
    .placeholder(Ansi::new().dimmed())
    .valid(Ansi::new().fg_color(Some(clap::builder::styling::Color::Ansi(AnsiColor::Green))));

/// Styles help text the way [`STYLES`] styles clap's part of it, or leaves it plain: one
/// rendering of a page is plain (`--json`, a pipe, `NO_COLOR`, …) and one styled.
#[derive(Debug, Clone, Copy)]
struct Paint {
    ansi: bool,
}

impl Paint {
    fn with(self, style: &Ansi, text: &str) -> String {
        if self.ansi {
            format!("{}{text}{}", style.render(), style.render_reset())
        } else {
            text.to_owned()
        }
    }

    /// A heading, as clap styles `Usage:` and `Options:`.
    fn header(self, text: &str) -> String {
        self.with(STYLES.get_header(), text)
    }

    /// A command or flag, as clap styles `--json`.
    fn literal(self, text: &str) -> String {
        self.with(STYLES.get_literal(), text)
    }

    /// The usage heading, as clap styles a command's `Usage:`.
    fn usage(self, text: &str) -> String {
        self.with(STYLES.get_usage(), text)
    }
}
/// What the discovery line starts with.
const AGENTS: &str = "Agents: ";
/// Between the discovery line's commands.
const DISCOVERY_SEPARATOR: &str = " · ";

/// A command bare help names for agents.
#[derive(Debug, Clone, Copy)]
pub struct Discovery {
    /// The argv, `omaddons` first, exactly as the line shows it.
    pub argv: &'static [&'static str],
    /// What an agent gets from it (`help --json` only).
    pub purpose: &'static str,
}

/// The discovery trio (vision §5.10), in the order bare help names it. Each entry parses
/// and runs a command whose `COMMANDS` row does not mutate (tests); bare help builds no
/// registry, so it reads no file.
pub const DISCOVERY: &[Discovery] = &[
    Discovery {
        argv: &["omaddons", "capabilities", "--json"],
        purpose: "Everything this build can do, as data: commands, flags, exit codes, error codes, limits and sources (offline; writes nothing)",
    },
    Discovery {
        argv: &["omaddons", "robot-docs", "guide"],
        purpose: "The agent guide in Markdown, the same text as the omaddons skill: the core loop, the safety rules and the recipes",
    },
    Discovery {
        argv: &["omaddons", "doctor", "--json"],
        purpose: "What is broken in what omaddons manages, with evidence and an exact remediation for each finding",
    },
];

/// Help or version, as data and as text.
#[derive(Debug)]
pub struct HelpOutcome {
    command: &'static str,
    data: HelpData,
    /// The page without any escape sequence.
    text: String,
    /// The same page styled, shown when the output policy allows color on stdout.
    styled: String,
}

/// The `data` of a help or version envelope.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(untagged)]
pub enum HelpData {
    Bare(BareHelp),
    Command(CommandHelp),
    Version(VersionInfo),
}

/// Bare `omaddons`.
#[derive(Debug, Serialize, JsonSchema)]
pub struct BareHelp {
    pub name: String,
    pub version: String,
    pub usage: String,
    pub commands: Vec<CommandInfo>,
    pub global_flags: Vec<FlagInfo>,
    pub exit_codes: Vec<ExitInfo>,
    /// The commands an agent with no prior knowledge starts from (vision §5.10), in the
    /// order the human line names them.
    pub discovery: Vec<DiscoveryEntry>,
}

/// One command bare help names for agents.
#[derive(Debug, Serialize, JsonSchema)]
pub struct DiscoveryEntry {
    /// `argv`, shell-quoted, as the human line shows it.
    pub command: String,
    pub argv: Vec<String>,
    /// What an agent gets from it.
    pub purpose: String,
}

impl From<&Discovery> for DiscoveryEntry {
    fn from(entry: &Discovery) -> Self {
        Self {
            command: shell_words::join(entry.argv),
            argv: entry.argv.iter().map(|word| (*word).to_owned()).collect(),
            purpose: entry.purpose.to_owned(),
        }
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct CommandInfo {
    pub name: String,
    pub about: String,
    pub aliases: Vec<String>,
    /// What it is for: `look`, `change` or `setup`.
    pub group: Group,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct FlagInfo {
    /// The long name without dashes.
    pub name: String,
    pub short: Option<String>,
    pub takes_value: bool,
    pub about: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ExitInfo {
    pub code: u8,
    pub name: String,
    pub meaning: String,
    /// `null` for success, and for `failed`, where `error.retryable` decides.
    pub retryable: Option<bool>,
}

/// `omaddons help <command>`.
#[derive(Debug, Serialize, JsonSchema)]
pub struct CommandHelp {
    /// The command's canonical path (`install`, `targets add`, `auth github`), as the
    /// envelope's `command` names it (rule 5); an alias asked for names its command.
    pub command: String,
    pub about: String,
    pub usage: String,
    pub args: Vec<ArgInfo>,
    pub flags: Vec<FlagInfo>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ArgInfo {
    pub name: String,
    pub required: bool,
    pub about: String,
}

/// `omaddons --version`.
#[derive(Debug, Serialize, JsonSchema)]
pub struct VersionInfo {
    pub version: String,
}

impl Outcome for HelpOutcome {
    type Data = HelpData;

    fn command(&self) -> &'static str {
        self.command
    }

    fn data(&self) -> &HelpData {
        &self.data
    }

    /// The styled page where the output policy allows color on stdout (a terminal, no
    /// `NO_COLOR`, `--no-color`, `TERM=dumb` or CI), else the plain one.
    fn render_human(&self, w: &mut dyn Write, style: &Style) -> std::io::Result<()> {
        let page = if style.color {
            &self.styled
        } else {
            &self.text
        };
        w.write_all(page.as_bytes())
    }
}

/// A short meaning per exit code for the human table, short enough that every row of
/// [`exit_table`] stays under 80 columns; the JSON carries the full one.
pub(crate) const fn short_meaning(exit: Exit) -> &'static str {
    match exit {
        Exit::Ok => "Success, including partial success (partial: true)",
        Exit::Failed => "Runtime failure; see error.retryable",
        Exit::Usage => "Unknown command or flag, bad argument, bad ref syntax",
        Exit::NotFound => "Unknown or ambiguous target, ref, addon, plan or transaction",
        Exit::Refused => "Safety refusal; fix names the flag or command",
        Exit::Locked => "Lock held past the acquire timeout; retryable",
        Exit::CheckFailed => "An opt-in assertion (--check, --strict) failed",
    }
}

fn flags(cmd: &clap::Command) -> Vec<FlagInfo> {
    cmd.get_arguments()
        .filter(|a| !a.is_positional())
        .filter_map(|a| {
            Some(FlagInfo {
                name: a.get_long()?.to_owned(),
                short: a.get_short().map(String::from),
                takes_value: a.get_action().takes_values(),
                about: a.get_help().map(ToString::to_string).unwrap_or_default(),
            })
        })
        .collect()
}

/// One line per group that has commands, in group order: the label, then the command
/// names. A line that would reach `MAX_WIDTH` columns continues on the next, under the
/// first name.
fn render_groups(commands: &[(&str, Group)], paint: Paint) -> String {
    let mut text = String::new();
    for group in Group::ALL {
        let names: Vec<&str> = commands
            .iter()
            .filter(|(_, g)| *g == group)
            .map(|(name, _)| *name)
            .collect();
        let label = group.label();
        let pad = " ".repeat(GROUP_COLUMN.saturating_sub(label.chars().count()));
        let lead = Lead {
            plain: &format!("{label}{pad}"),
            shown: &format!("{}{pad}", paint.literal(label)),
        };
        text.push_str(&wrap_painted(
            MAX_WIDTH,
            lead,
            &names,
            "  ",
            GROUP_COLUMN,
            &|name| paint.literal(name),
        ));
    }
    text
}

/// What a wrapped line starts with: its text as counted, and as printed (styled).
#[derive(Clone, Copy)]
struct Lead<'a> {
    plain: &'a str,
    shown: &'a str,
}

/// `items` after `lead`, joined by `separator`, wrapped so every line is narrower than
/// `MAX_WIDTH` characters: an item that would make the line reach `MAX_WIDTH` starts the
/// next line, after `indent` spaces, and the separator before it is dropped. An item is
/// never split, so one wider than a line stands on a line of its own. Widths are
/// characters, not bytes (`·` is two bytes). No items, no text. Every line ends with
/// `\n`.
fn wrap<S: AsRef<str>>(lead: &str, items: &[S], separator: &str, indent: usize) -> String {
    let lead = Lead {
        plain: lead,
        shown: lead,
    };
    wrap_painted(MAX_WIDTH, lead, items, separator, indent, &|item| {
        item.to_owned()
    })
}

/// [`wrap`] narrower than `limit` columns, printing `lead.shown` and each item through
/// `paint` while counting widths on the plain text, so styling never moves a break.
fn wrap_painted<S: AsRef<str>>(
    limit: usize,
    lead: Lead<'_>,
    items: &[S],
    separator: &str,
    indent: usize,
    paint: &dyn Fn(&str) -> String,
) -> String {
    let width = |text: &str| text.chars().count();
    let Some((first, rest)) = items.split_first() else {
        return String::new();
    };
    let mut text = String::new();
    let mut columns = width(lead.plain) + width(first.as_ref());
    let mut line = format!("{}{}", lead.shown, paint(first.as_ref()));
    for item in rest {
        let item = item.as_ref();
        if columns + width(separator) + width(item) >= limit {
            text.push_str(&line);
            text.push('\n');
            line = format!("{:indent$}{}", "", paint(item));
            columns = indent + width(item);
        } else {
            line.push_str(separator);
            line.push_str(&paint(item));
            columns += width(separator) + width(item);
        }
    }
    text.push_str(&line);
    text.push('\n');
    text
}

/// The words of a sentence for [`wrap`]: split at whitespace, except inside a backtick
/// span, so a quoted command never breaks across lines. A run of whitespace counts as one
/// space; an unclosed span runs to the end.
fn prose_words(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    for c in text.chars() {
        if c == '`' {
            quoted = !quoted;
        }
        if c.is_whitespace() && !quoted {
            if !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
        } else {
            word.push(c);
        }
    }
    if !word.is_empty() {
        words.push(word);
    }
    words
}

/// Bare help's text for `commands` (name and group, in display order) and the commands
/// it names for agents.
fn bare_text(
    usage: &str,
    commands: &[(&str, Group)],
    discovery: &[Discovery],
    paint: Paint,
) -> String {
    let mut text = String::new();
    text.push_str("World of Warcraft addon manager for Linux and an Omarchy widget\n\n");
    text.push_str(&format!("{} {usage}\n\n", paint.usage("Usage:")));
    text.push_str(&render_groups(commands, paint));
    text.push('\n');
    text.push_str(&guidance(commands, discovery));
    text.push_str(&format!("\n{}\n", paint.header("Exit codes:")));
    text.push_str(&exit_table());
    text.push_str(&format!("\n{}\n", paint.literal(FULL_POINTER)));
    text
}

/// What bare help ends with: the one line that points at the full page.
const FULL_POINTER: &str = "omaddons --help lists every command with what it does";

/// Where to start and the commands for agents: the "Start with" sentence and the
/// discovery line, wrapped under 80 columns, every line ending in a newline.
fn guidance(commands: &[(&str, Group)], discovery: &[Discovery]) -> String {
    let mut sentence = String::new();
    if commands.iter().any(|(name, _)| *name == "status") {
        sentence.push_str("Start with `omaddons status`. ");
    }
    sentence.push_str(
        "Run `omaddons help <command>` for details; add --json for one JSON envelope on stdout.",
    );
    let mut text = wrap("", &prose_words(&sentence), " ", 0);
    let shown: Vec<String> = discovery
        .iter()
        .map(|entry| shell_words::join(entry.argv))
        .collect();
    text.push_str(&wrap(
        AGENTS,
        &shown,
        DISCOVERY_SEPARATOR,
        AGENTS.chars().count(),
    ));
    text
}

/// A group's heading on the full page: what its commands have in common.
fn group_heading(group: Group) -> String {
    match group {
        Group::Look => format!("{} (changes no addon):", group.label()),
        Group::Change => format!("{} (preview any with --dry-run):", group.label()),
        Group::Setup => format!("{}:", group.label()),
    }
}

/// The full top-level page (`omaddons --help`, `omaddons help`), at most `width`
/// columns: every top-level command under its group with its aliases and summary, every
/// global option, the environment omaddons reads, where to start, the exit codes and
/// where to learn more.
fn full_text(commands: &[CommandInfo], root: &clap::Command, width: usize, paint: Paint) -> String {
    let mut text = format!(
        "omaddons {} · World of Warcraft addon manager for Linux\n\n{} omaddons [OPTIONS] <COMMAND>\n",
        env!("CARGO_PKG_VERSION"),
        paint.usage("Usage:")
    );
    let named = |command: &CommandInfo| {
        std::iter::once(command.name.as_str())
            .chain(command.aliases.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let column = commands
        .iter()
        .map(|command| named(command).chars().count())
        .max()
        .unwrap_or(0);
    for group in Group::ALL {
        let rows: Vec<&CommandInfo> = commands.iter().filter(|c| c.group == group).collect();
        if rows.is_empty() {
            continue;
        }
        text.push_str(&format!("\n{}\n", paint.header(&group_heading(group))));
        for command in rows {
            let names = named(command);
            let line = fit(&format!("  {names:<column$}  {}", command.about), width);
            // The name is styled after fitting, so styling never moves a break.
            let rest = line.get(2 + command.name.len()..).unwrap_or_default();
            text.push_str(&format!("  {}{rest}\n", paint.literal(&command.name)));
        }
    }
    let options: Vec<clap::Arg> = root
        .get_arguments()
        .filter(|arg| !arg.is_hide_set())
        .cloned()
        .collect();
    text.push_str(&format!("\n{}\n", paint.header("Global options:")));
    text.push_str(&global_options(options, paint));
    text.push_str(&format!("\n{}\n", paint.header("Environment:")));
    text.push_str(&environment(width));
    text.push('\n');
    let listed: Vec<(&str, Group)> = commands
        .iter()
        .map(|c| (c.name.as_str(), c.group))
        .collect();
    text.push_str(&guidance(&listed, DISCOVERY));
    text.push_str(&format!("\n{}\n", paint.header("Exit codes:")));
    text.push_str(&exit_table());
    text.push_str(&format!(
        "\n{} {LEARN_MORE} · README.md\n",
        paint.header("Learn more:")
    ));
    text
}

/// The variables omaddons reads, one line each with what it does (the release ones),
/// then the ones only debug builds read; the test harness's own variables are never read
/// by the binary, so they are not listed.
fn environment(width: usize) -> String {
    use crate::contract::env::{ENV_VARS, EnvKind};
    let rows: Vec<Vec<String>> = ENV_VARS
        .iter()
        .filter(|var| var.kind == EnvKind::Release)
        .map(|var| vec![format!("  {}", var.name), var.description.to_owned()])
        .collect();
    let mut text = fit(table(&rows).trim_end(), width);
    let debug: Vec<&str> = ENV_VARS
        .iter()
        .filter(|var| var.kind == EnvKind::Debug)
        .map(|var| var.name)
        .collect();
    if !debug.is_empty() {
        text.push('\n');
        text.push_str(&fit(
            &format!("  Debug builds also read {}", debug.join(", ")),
            width,
        ));
    }
    text.push('\n');
    text
}

/// The general exit codes as a table, one line each, every line ending in a newline: bare
/// help and `capabilities` print the same one.
pub(crate) fn exit_table() -> String {
    let rows: Vec<Vec<String>> = Exit::ALL
        .iter()
        .map(|e| {
            vec![
                format!("  {}", e.code()),
                e.name().to_owned(),
                short_meaning(*e).to_owned(),
            ]
        })
        .collect();
    table(&rows)
}

/// Bare `omaddons`: what it is, the verbs by group, and the exit codes.
pub fn bare() -> HelpOutcome {
    top_level(Depth::Short)
}

/// `omaddons --help` and `omaddons help`: the full top-level page. Its data is bare
/// help's, so `help --json` is the same either way.
pub fn full() -> HelpOutcome {
    top_level(Depth::Long)
}

/// The top-level page at `depth`: concise (bare help) or full.
fn top_level(depth: Depth) -> HelpOutcome {
    let cmd = Cli::command();
    // A subcommand missing from the registry is a bug the completeness test catches;
    // it is still listed, last.
    let place = |name: &str| group_of(name).unwrap_or((Group::Setup, usize::MAX));
    let mut commands: Vec<CommandInfo> = cmd
        .get_subcommands()
        .map(|c| CommandInfo {
            name: c.get_name().to_owned(),
            about: c.get_about().map(ToString::to_string).unwrap_or_default(),
            aliases: c.get_visible_aliases().map(str::to_owned).collect(),
            group: place(c.get_name()).0,
        })
        .collect();
    commands.sort_by_key(|c| {
        let (group, at) = place(&c.name);
        (group, at, c.name.clone())
    });
    let exit_codes: Vec<ExitInfo> = Exit::ALL
        .iter()
        .map(|exit| ExitInfo {
            code: exit.code(),
            name: exit.name().to_owned(),
            meaning: exit.meaning().to_owned(),
            retryable: exit.retryable(),
        })
        .collect();
    let usage = "omaddons [OPTIONS] <COMMAND>".to_owned();
    let listed: Vec<(&str, Group)> = commands
        .iter()
        .map(|c| (c.name.as_str(), c.group))
        .collect();
    let (text, styled) = match depth {
        Depth::Short => (
            bare_text(&usage, &listed, DISCOVERY, Paint { ansi: false }),
            bare_text(&usage, &listed, DISCOVERY, Paint { ansi: true }),
        ),
        Depth::Long => {
            let width = help_width();
            (
                full_text(&commands, &cmd, width, Paint { ansi: false }),
                full_text(&commands, &cmd, width, Paint { ansi: true }),
            )
        }
    };

    HelpOutcome {
        command: "help",
        styled,
        data: HelpData::Bare(BareHelp {
            name: "omaddons".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            usage,
            commands,
            global_flags: flags(&cmd),
            exit_codes,
            discovery: DISCOVERY.iter().map(DiscoveryEntry::from).collect(),
        }),
        text,
    }
}

/// How much of a command's page is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Depth {
    /// `-h`: the about, the usage, one line per own option, a few examples, and one line
    /// naming the global options most command lines need.
    Short,
    /// `--help` and `help <command>`: the long about, every own option's whole help, all
    /// examples, related commands, every global option on one line, and where to learn
    /// more.
    Long,
}

impl Depth {
    /// The depth `argv` asks for: `--help` before any `--` is the whole page, `-h` the
    /// compact one.
    pub fn of(argv: &[String]) -> Self {
        let long = argv
            .iter()
            .skip(1)
            .take_while(|a| *a != "--")
            .any(|a| a == "--help");
        if long { Self::Long } else { Self::Short }
    }
}

/// `omaddons help [<command>]`, the whole page; an unknown name is a `usage` error
/// suggesting the closest command.
pub fn for_command(name: Option<&str>, argv: &[String]) -> Result<HelpOutcome, Error> {
    at_depth(name, argv, Depth::Long)
}

/// `-h` or `--help` on a command line: its page at the depth `argv` asks for.
pub fn for_flag(name: Option<&str>, argv: &[String]) -> Result<HelpOutcome, Error> {
    at_depth(name, argv, Depth::of(argv))
}

/// A command's help at `depth`; for no name, the top-level page: bare help short, the
/// full page long.
fn at_depth(name: Option<&str>, argv: &[String], depth: Depth) -> Result<HelpOutcome, Error> {
    let Some(name) = name else {
        return Ok(top_level(depth));
    };
    let mut root = Cli::command();
    root.build();
    // A nested path such as `targets add` or `auth github` walks one level per word. A
    // word that is a possible value of the command's next positional (`auth github set`)
    // stays on that command, and so does every word after it. An unknown word is
    // suggested among the subcommands and values the command it follows takes.
    let mut sub = &mut root;
    let mut walked: Vec<&str> = Vec::new();
    // The canonical path of the command reached: each subcommand's own name, whatever
    // alias was typed, and never a positional value (`auth github set` is `auth github`).
    let mut path: Vec<String> = Vec::new();
    let mut values = 0;
    for part in name.split_whitespace() {
        if values == 0 && sub.find_subcommand(part).is_some() {
            walked.push(part);
            let Some(next) = sub.find_subcommand_mut(part) else {
                return Err(Error::internal(format!(
                    "{part} vanished from the command tree"
                )));
            };
            path.push(next.get_name().to_owned());
            sub = next;
            continue;
        }
        let positional = sub.get_positionals().nth(values);
        let possible: Vec<String> = positional
            .map(|arg| {
                arg.get_possible_values()
                    .iter()
                    .filter(|v| !v.is_hide_set())
                    .map(|v| v.get_name().to_owned())
                    .collect()
            })
            .unwrap_or_default();
        if possible.iter().any(|v| v == part) {
            values += 1;
            walked.push(part);
            continue;
        }
        let names: Vec<String> = if values == 0 {
            sub.get_subcommands()
                .map(|c| c.get_name().to_owned())
                .collect()
        } else {
            Vec::new()
        };
        // Past the first word: the help of the command walked so far.
        let parent = || {
            let mut argv = vec!["omaddons", "help"];
            argv.extend(&walked);
            (Suggestion::command(argv), Vec::new())
        };
        let candidates: Vec<String> = names.iter().chain(&possible).cloned().collect();
        let (fix, alternatives) =
            crate::cli::suggest(argv, part, &candidates).unwrap_or_else(parent);
        let walked = walked.join(" ");
        let message = match (walked.is_empty(), positional) {
            (true, _) => format!("there is no command {name:?}"),
            _ if !names.is_empty() => format!(
                "there is no command {name:?}: {walked} has {}",
                names.join(", ")
            ),
            (false, Some(arg)) if !possible.is_empty() => format!(
                "there is no command {name:?}: {walked} takes {} ({})",
                arg.get_value_names()
                    .and_then(|v| v.first())
                    .map_or_else(|| arg.get_id().to_string(), ToString::to_string),
                possible.join(", ")
            ),
            _ => format!("there is no command {name:?}: {walked} takes no subcommands"),
        };
        let mut error = Error::new(ErrorCode::Usage, message, fix);
        error.alternatives = alternatives;
        return Err(error);
    }
    let width = help_width();
    let text = page(sub, &path.join(" "), depth, width, Paint { ansi: false });
    let styled = page(sub, &path.join(" "), depth, width, Paint { ansi: true });
    let usage = sub
        .render_usage()
        .to_string()
        .trim_start_matches("Usage: ")
        .trim()
        .to_owned();
    let args = sub
        .get_arguments()
        .filter(|a| a.is_positional())
        .map(|a| ArgInfo {
            name: a.get_id().to_string(),
            required: a.is_required_set(),
            about: a.get_help().map(ToString::to_string).unwrap_or_default(),
        })
        .collect();
    Ok(HelpOutcome {
        command: "help",
        data: HelpData::Command(CommandHelp {
            command: path.join(" "),
            about: sub.get_about().map(ToString::to_string).unwrap_or_default(),
            usage,
            args,
            flags: flags(sub),
        }),
        text,
        styled,
    })
}

/// What `-h` names of the global options, in this order: the ones a command line most
/// often needs. `--help` lists every one.
pub(crate) const GLOBAL_SUMMARY: &[&str] = &[
    "-t <TARGET>",
    "--json",
    "--dry-run",
    "-v",
    "--quiet",
    "--no-color",
    "--offline",
];
/// Where every complete page points last.
const LEARN_MORE: &str = "omaddons robot-docs guide";
/// The widest example command that keeps its description on its own line's right.
const EXAMPLE_COMMAND_MAX: usize = 44;
/// The fewest columns a wrapped line's continuation keeps for its words: a hanging
/// indent deeper than that falls back to four past the line's own indent.
const HANG_MIN_ROOM: usize = 20;

/// The width clap wraps this run's help at: the terminal's, else `COLUMNS`, else 100,
/// never more than [`HELP_MAX_COLUMNS`]. omaddons reads no width itself (no terminal call,
/// no variable): it lets clap wrap a probe of one-letter words and measures the widest
/// line, which is the width or one less.
fn help_width() -> usize {
    let probe = vec!["x"; HELP_MAX_COLUMNS].join(" ");
    clap::Command::default()
        .name("omaddons")
        .max_term_width(HELP_MAX_COLUMNS)
        .help_template("{about}")
        .about(probe)
        .render_help()
        .to_string()
        .lines()
        .map(|line| line.trim_end().chars().count())
        .max()
        .unwrap_or(HELP_MAX_COLUMNS)
}

/// A command's page at `depth`, `width` columns at most. It is rendered from a copy of
/// `sub`, so the tree that `help --json`, `capabilities` and the contract read never
/// changes for presentation:
///
/// - the global options leave the options list for a block of their own after the
///   command's: one summary line under `-h`, one line each under `--help`, `--if-match`
///   only where the command binds and `--robot` (`--json` by another name) nowhere;
/// - an enumeration the option's prose already spells out is not repeated as clap's
///   `[possible values]`;
/// - the command's aliases follow its usage, and its examples, notes and related
///   commands ([`super::help_extras`]) follow its options, wrapped here at `width` with
///   hanging indents (clap would wrap them back to column 0).
fn page(sub: &clap::Command, path: &str, depth: Depth, width: usize, paint: Paint) -> String {
    let usage = sub.clone().render_usage().to_string();
    let usage = usage.trim_start_matches("Usage: ").trim_end().to_owned();
    let globals: Vec<clap::Arg> = sub
        .get_arguments()
        .filter(|arg| arg.is_global_set())
        .cloned()
        .collect();
    let mut copy = sub.clone();
    for arg in &globals {
        copy = copy.mut_arg(arg.get_id().clone(), |arg| arg.hide(true));
    }
    let spelled_out: Vec<clap::Id> = copy
        .get_arguments()
        .filter(|arg| !arg.is_global_set() && lists_its_values(arg))
        .map(|arg| arg.get_id().clone())
        .collect();
    for id in spelled_out {
        copy = copy.mut_arg(id, |arg| arg.hide_possible_values(true));
    }
    let aliases: Vec<&str> = sub.get_visible_aliases().collect();
    let aliases = if aliases.is_empty() {
        String::new()
    } else {
        format!("\n{} {}", paint.header("Aliases:"), aliases.join(", "))
    };
    let shows_args = copy.get_arguments().any(|arg| !arg.is_hide_set())
        || copy.get_subcommands().any(|cmd| !cmd.is_hide_set());
    let between = if shows_args { "\n\n{all-args}" } else { "" };
    copy = copy
        .help_template(format!(
            "{{about-with-newline}}\n{{usage-heading}} {{usage}}{aliases}{between}"
        ))
        .override_usage(fit_usage(&usage, width));
    let rendered = match depth {
        Depth::Short => copy.render_help(),
        Depth::Long => copy.render_long_help(),
    };
    let rendered = if paint.ansi {
        rendered.ansi().to_string()
    } else {
        rendered.to_string()
    };
    let mut text = rendered.trim_end().to_owned();
    text.push_str("\n\n");
    let extras = super::help_extras::of(path);
    let after = match depth {
        Depth::Short => extras.map(|extras| examples_block(extras.examples)),
        Depth::Long => long_after(sub.get_after_long_help().map(ToString::to_string), extras),
    };
    if let Some(after) = after {
        text.push_str(&headed(&fit(&after, width), paint));
        text.push_str("\n\n");
    }
    let binds = super::precondition::BINDING_COMMANDS.contains(&path);
    match depth {
        Depth::Short => {
            let lead = Lead {
                plain: "Global: ",
                shown: &format!("{} ", paint.header("Global:")),
            };
            let mut items: Vec<&str> = GLOBAL_SUMMARY.to_vec();
            items.push("(--help lists them all)");
            text.push_str(&wrap_painted(width + 1, lead, &items, "  ", 8, &|item| {
                item.to_owned()
            }));
        }
        Depth::Long => {
            // `--robot` never (`--json` by another name), `--if-match` only where the
            // command binds.
            let listed: Vec<clap::Arg> = globals
                .into_iter()
                .filter(|arg| {
                    let id = arg.get_id().as_str();
                    id != "robot" && (binds || id != "if_match")
                })
                .collect();
            text.push_str(&format!("{}\n", paint.header("Global options:")));
            text.push_str(&global_options(listed, paint));
            text.push_str(&format!("\n{} {LEARN_MORE}\n", paint.header("Learn more:")));
        }
    }
    text
}

/// `listed` options one line each with their short help, in clap's own two columns at
/// clap's width.
fn global_options(listed: Vec<clap::Arg>, paint: Paint) -> String {
    // A clap command only to render the block (`default()`, not the `new` constructor the
    // single-writer scan reads as a process spawn).
    let rendered = clap::Command::default()
        .name("omaddons")
        .styles(STYLES)
        .max_term_width(HELP_MAX_COLUMNS)
        .disable_help_flag(true)
        .disable_version_flag(true)
        .args(listed)
        .help_template("{options}")
        .render_help();
    let block = if paint.ansi {
        rendered.ansi().to_string()
    } else {
        rendered.to_string()
    };
    let mut block = block.trim_end().to_owned();
    block.push('\n');
    block
}

/// The usage as clap prints it after `Usage: `, each of its lines wrapped so the page
/// stays within `width` columns, continuing four past where the command starts: clap
/// never wraps a usage line (`copy`'s is 71 columns).
fn fit_usage(usage: &str, width: usize) -> String {
    const HEADING: &str = "Usage: ";
    let mut lines = Vec::new();
    for (at, line) in usage.lines().enumerate() {
        // The first line follows the heading clap prints; the others carry their indent.
        let lead = if at == 0 {
            Lead {
                plain: HEADING,
                shown: "",
            }
        } else {
            Lead {
                plain: "       ",
                shown: "       ",
            }
        };
        let wrapped = wrap_painted(
            width + 1,
            lead,
            &prose_words(line),
            " ",
            HEADING.len() + 4,
            &|word| word.to_owned(),
        );
        lines.extend(wrapped.lines().map(str::to_owned));
    }
    lines.join("\n")
}

/// `text` with its block headings styled: an `Examples:` line, and the `See also:` that
/// starts a line.
fn headed(text: &str, paint: Paint) -> String {
    text.lines()
        .map(|line| {
            if line == "Examples:" {
                paint.header(line)
            } else if let Some(rest) = line.strip_prefix("See also:") {
                format!("{}{rest}", paint.header("See also:"))
            } else {
                line.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `text` with every line wider than `width` columns wrapped between words. A
/// continuation is indented to where the line's last column starts (the text after its
/// last run of two or more spaces: an example's description, a table's meaning), or, for
/// a line without columns, to the line's own indent; a column too deep to leave
/// [`HANG_MIN_ROOM`] or the line's longest word falls back to four past the line's
/// indent. A word wider than the room stands on a line of its own, as clap does. Widths
/// are characters. No trailing newline.
pub(crate) fn fit(text: &str, width: usize) -> String {
    let mut lines = Vec::new();
    for line in text.lines() {
        if line.chars().count() <= width {
            lines.push(line.to_owned());
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        let longest = prose_words(line)
            .iter()
            .map(|word| word.chars().count())
            .max()
            .unwrap_or(0);
        let room = HANG_MIN_ROOM.max(longest);
        let (lead, rest, hang) = match last_column(line) {
            Some(at) if at + room <= width => {
                let split = line.char_indices().nth(at).map_or(line.len(), |(i, _)| i);
                (&line[..split], &line[split..], at)
            }
            Some(_) if indent + 4 + room <= width => (&line[..indent], &line[indent..], indent + 4),
            _ => (&line[..indent], &line[indent..], indent),
        };
        let lead = Lead {
            plain: lead,
            shown: lead,
        };
        let wrapped = wrap_painted(width + 1, lead, &prose_words(rest), " ", hang, &|word| {
            word.to_owned()
        });
        lines.extend(wrapped.lines().map(str::to_owned));
    }
    lines.join("\n")
}

/// Where `line`'s last column starts, in characters: after its last run of two or more
/// spaces past the first word. `None` for a line without one.
fn last_column(line: &str) -> Option<usize> {
    let chars: Vec<char> = line.chars().collect();
    let start = chars.iter().position(|c| *c != ' ')?;
    let mut column = None;
    let mut at = start;
    while at < chars.len() {
        if chars[at] == ' ' && chars.get(at + 1) == Some(&' ') {
            let mut end = at;
            while chars.get(end) == Some(&' ') {
                end += 1;
            }
            if end < chars.len() {
                column = Some(end);
            }
            at = end;
        } else {
            at += 1;
        }
    }
    column
}

/// Whether `arg`'s own help already names every value it takes, so clap's
/// `[possible values: …]` would say the list twice.
fn lists_its_values(arg: &clap::Arg) -> bool {
    if !arg.get_action().takes_values() {
        return false;
    }
    let values: Vec<String> = arg
        .get_possible_values()
        .iter()
        .filter(|value| !value.is_hide_set())
        .map(|value| value.get_name().to_owned())
        .collect();
    let help: String = [arg.get_help(), arg.get_long_help()]
        .into_iter()
        .flatten()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(" ");
    let words: std::collections::BTreeSet<&str> = help
        .split(|c: char| !(c.is_alphanumeric() || c == '-' || c == '_'))
        .collect();
    !values.is_empty() && values.iter().all(|value| words.contains(value.as_str()))
}

/// What `--help` shows after the options: the table's examples when the command's own
/// long text has none, that text (its examples and notes), and the related commands.
fn long_after(own: Option<String>, extras: Option<&super::help_extras::Extras>) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(extras) = extras
        && !own.as_ref().is_some_and(|text| text.contains("Examples:"))
    {
        parts.push(examples_block(extras.examples));
    }
    parts.extend(own);
    if let Some(extras) = extras
        && !extras.see_also.is_empty()
    {
        parts.push(format!("See also: {}", extras.see_also.join(", ")));
    }
    (!parts.is_empty()).then(|| parts.join("\n\n"))
}

/// An `Examples:` block: each command two columns in, its description aligned beside the
/// commands that fit [`EXAMPLE_COMMAND_MAX`] (on the next line, in the same column, for a
/// longer one). Unwrapped: a page [`fit`]s it to its width, hanging each description at
/// its column. No trailing newline.
pub(crate) fn examples_block(examples: &[(&str, &str)]) -> String {
    let columns = |text: &str| text.chars().count();
    let width = examples
        .iter()
        .map(|(command, _)| columns(command))
        .filter(|width| *width <= EXAMPLE_COMMAND_MAX)
        .max()
        .unwrap_or(0);
    let mut lines = vec!["Examples:".to_owned()];
    for (command, what) in examples {
        if what.is_empty() {
            lines.push(format!("  {command}"));
        } else if columns(command) <= EXAMPLE_COMMAND_MAX {
            lines.push(format!("  {command:<width$}  {what}"));
        } else {
            lines.push(format!("  {command}"));
            lines.push(format!("{:indent$}{what}", "", indent = width + 4));
        }
    }
    lines.join("\n")
}

/// `omaddons --version`.
pub fn version() -> HelpOutcome {
    let version = env!("CARGO_PKG_VERSION").to_owned();
    let text = format!("omaddons {version}\n");
    HelpOutcome {
        command: "version",
        styled: text.clone(),
        text,
        data: HelpData::Version(VersionInfo { version }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `help <path> --json` names the command by its canonical path (rule 5): a leaf name
    /// alone is ambiguous (`add` is also `install`'s alias), and a value or an alias typed
    /// never enters it.
    #[test]
    fn nested_help_names_the_canonical_path() {
        for (asked, expected) in [
            ("targets add", "targets add"),
            ("auth github", "auth github"),
            ("auth github set", "auth github"),
            ("backup restore", "backup restore"),
            ("config set", "config set"),
            ("errors dismiss", "errors dismiss"),
            ("doctor fix", "doctor fix"),
            ("install", "install"),
            ("add", "install"),
            ("backup", "backup"),
        ] {
            let outcome = for_command(Some(asked), &[]).unwrap();
            let HelpData::Command(help) = &outcome.data else {
                panic!("{asked:?}: not a command's help"); // ubs:ignore — a table case's own failure
            };
            assert_eq!(help.command, expected, "help {asked} --json: data.command");
        }
    }

    #[test]
    fn bare_help_lists_commands_and_every_exit_code() {
        let help = bare();
        assert!(help.text.contains("Usage: omaddons [OPTIONS] <COMMAND>"));
        assert!(help.text.contains("help"));
        for exit in Exit::ALL {
            assert!(help.text.contains(exit.name()), "{}", exit.name());
        }
        assert!(
            help.text.contains("Start with `omaddons status`"),
            "status is where to start"
        );
        assert!(
            help.text.lines().count() < 30,
            "concise: {} lines",
            help.text.lines().count()
        );
        let data = serde_json::to_value(&help.data).unwrap();
        let codes: Vec<u64> = data["exit_codes"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|e| e["code"].as_u64())
            .collect();
        assert_eq!(codes, [0, 1, 2, 3, 4, 5, 6]);
        assert_eq!(data["exit_codes"][5]["retryable"], true);
        assert!(data["exit_codes"][1]["retryable"].is_null());
        let flags = data["global_flags"].as_array().unwrap();
        assert!(
            flags
                .iter()
                .any(|f| f["name"] == "json" && f["takes_value"] == false)
        );
        assert!(
            flags
                .iter()
                .any(|f| f["name"] == "target" && f["short"] == "t")
        );
    }

    /// Bare help names the discovery trio for agents, after the wrapped "Start with"
    /// sentence and before the exit codes, and `--json` carries the same as `discovery`.
    #[test]
    fn bare_help_names_the_discovery_trio() {
        let help = bare();
        let tail = "Start with `omaddons status`. Run `omaddons help <command>` for details; add\n\
                    --json for one JSON envelope on stdout.\n\
                    Agents: omaddons capabilities --json · omaddons robot-docs guide\n        \
                    omaddons doctor --json\n\nExit codes:\n";
        assert!(help.text.contains(tail), "{}", help.text);
        let data = serde_json::to_value(&help.data).unwrap();
        let entries = data["discovery"].as_array().unwrap();
        assert_eq!(entries.len(), DISCOVERY.len());
        for (entry, declared) in entries.iter().zip(DISCOVERY) {
            assert_eq!(entry["argv"], serde_json::json!(declared.argv), "{entry}");
            assert_eq!(entry["argv"][0], "omaddons", "{entry}");
            assert_eq!(
                entry["command"],
                shell_words::join(declared.argv),
                "{entry}"
            );
            assert!(
                entry["purpose"].as_str().is_some_and(|p| !p.is_empty()),
                "{entry}"
            );
        }
        let again = bare();
        assert_eq!(help.text, again.text, "bare help renders the same bytes");
        assert_eq!(
            serde_json::to_string(&help.data).unwrap(),
            serde_json::to_string(&again.data).unwrap()
        );
        // Only bare help carries it.
        let one = serde_json::to_value(&for_command(Some("install"), &[]).unwrap().data).unwrap();
        assert!(one.get("discovery").is_none(), "{one}");
        assert!(
            serde_json::to_value(&version().data)
                .unwrap()
                .get("discovery")
                .is_none()
        );
    }

    /// Bare help never names a command this build does not have, nor one that changes
    /// something: each discovery argv parses and names a read-only `COMMANDS` row.
    #[test]
    fn every_discovery_argv_parses_to_a_read_only_command() {
        use clap::Parser;
        for (entry, row) in
            DISCOVERY
                .iter()
                .zip(["capabilities", "robot-docs guide", "doctor diagnose"])
        {
            let parsed = Cli::try_parse_from(entry.argv)
                .unwrap_or_else(|e| panic!("{:?} does not parse: {e}", entry.argv)); // ubs:ignore — a table case's own failure
            let name = super::super::command_name(parsed.command.as_ref());
            assert_eq!(name, row, "{:?}", entry.argv);
            let found = super::super::COMMANDS.iter().find(|c| c.name == name);
            assert!(
                found.is_some_and(|c| !c.mutates),
                "{:?}: {name} is no read-only row",
                entry.argv
            );
        }
    }

    /// One wrapping rule: items never split, separators dropped at a break, the
    /// continuation indented, widths in characters; prose breaks between words, never
    /// inside a backtick span.
    #[test]
    fn wrap_breaks_between_items_by_characters() {
        /// A case: its name, the lead, the items, the separator, the indent, the text.
        type Case<'a> = (&'a str, &'a str, Vec<&'a str>, &'a str, usize, &'a str);
        let (long, wide) = ("x".repeat(70), "w".repeat(90));
        let cases: &[Case<'_>] = &[
            ("no_items", "Lead ", vec![], " · ", 4, ""),
            (
                "one_line",
                "Agents: ",
                vec!["a", "b"],
                " · ",
                8,
                "Agents: a · b\n",
            ),
            (
                "breaks_before_the_item_that_reaches_the_width",
                "",
                vec![&long, "yyyyyyy", "z"],
                " · ",
                2,
                // 70 + 3 + 7 = 80 reaches MAX_WIDTH: `yyyyyyy` starts the next line.
                "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\n  yyyyyyy · z\n",
            ),
            (
                "an_item_wider_than_a_line_stands_alone",
                "",
                vec!["a", &wide],
                " ",
                0,
                "a\nwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwww\n",
            ),
        ];
        for (name, lead, items, separator, indent, expected) in cases {
            assert_eq!(
                wrap(lead, items, separator, *indent),
                *expected,
                "case {name}: {items:?}"
            );
        }
        // The middle dot is one column, two bytes: a byte count would break early.
        let dots = wrap("", &[&"x".repeat(74), "y"], " · ", 0);
        assert_eq!(dots.lines().count(), 1, "{dots}");
        assert_eq!(
            prose_words("Start  with `omaddons help <command>` now."),
            ["Start", "with", "`omaddons help <command>`", "now."],
        );
        assert_eq!(prose_words("an `unclosed span"), ["an", "`unclosed span"]);
    }

    #[test]
    fn groups_render_one_line_each_and_wrap_under_the_first_name() {
        let commands = [
            ("status", Group::Look),
            ("list", Group::Look),
            ("help", Group::Setup),
        ];
        assert_eq!(
            render_groups(&commands, Paint { ansi: false }),
            "Look      status  list\nSetup     help\n",
            "a group without commands is left out"
        );
        let long: Vec<(String, Group)> = (0..12)
            .map(|i| (format!("command{i:02}"), Group::Change))
            .collect();
        let long: Vec<(&str, Group)> = long.iter().map(|(n, g)| (n.as_str(), *g)).collect();
        let text = render_groups(&long, Paint { ansi: false });
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines.len() > 1, "{text}");
        assert!(lines[0].starts_with("Change    command00  command01"));
        assert!(
            lines[1].starts_with("          command"),
            "under the first name"
        );
        assert!(lines.iter().all(|l| l.len() < MAX_WIDTH), "{text}");
        assert_eq!(text.matches("command").count(), 12, "nothing lost");
    }

    #[test]
    fn bare_help_stays_one_screen_with_every_planned_command() {
        use Group::{Change, Look, Setup};
        // Every command through M3's backup, and M4's apply, plans and doctor, in the
        // planned layout; test-only, never live help.
        let planned = [
            ("status", Look),
            ("list", Look),
            ("info", Look),
            ("outdated", Look),
            ("search", Look),
            ("history", Look),
            ("plans", Look),
            ("net-log", Look),
            ("doctor", Look),
            ("export", Look),
            ("install", Change),
            ("update", Change),
            ("remove", Change),
            ("pin", Change),
            ("unpin", Change),
            ("adopt", Change),
            ("sync", Change),
            ("copy", Change),
            ("undo", Change),
            ("apply", Change),
            ("backup", Change),
            ("targets", Setup),
            ("config", Setup),
            ("auth", Setup),
            ("completions", Setup),
            ("robot-docs", Setup),
            ("capabilities", Setup),
            ("help", Setup),
        ];
        assert_eq!(planned.len(), 28);
        let text = bare_text(
            "omaddons [OPTIONS] <COMMAND>",
            &planned,
            DISCOVERY,
            Paint { ansi: false },
        );
        assert!(
            text.lines().count() < 30,
            "{} lines\n{text}",
            text.lines().count()
        );
        // Every line above the exit table, counted in characters (`·` is two bytes).
        let above = text.split("\nExit codes:").next().unwrap_or_default();
        for line in above.lines() {
            assert!(line.chars().count() < MAX_WIDTH, "{line:?} in\n{text}");
        }
        let groups = render_groups(&planned, Paint { ansi: false });
        assert!(groups.lines().all(|l| l.len() < MAX_WIDTH), "{groups}");
        assert!(
            groups.contains(
                "Change    install  update  remove  pin  unpin  adopt  sync  copy  undo  apply\n          backup"
            ),
            "{groups}"
        );
    }

    #[test]
    fn the_exit_table_stays_under_80_columns() {
        // `capabilities` prints the same table and holds every line under 80 columns.
        let text = exit_table();
        assert_eq!(text.lines().count(), Exit::ALL.len(), "{text}");
        for line in text.lines() {
            assert!(line.chars().count() < MAX_WIDTH, "{line:?}\n{text}");
        }
    }

    #[test]
    fn command_help_and_unknown_commands() {
        let help = for_command(Some("help"), &[]).unwrap();
        let data = serde_json::to_value(&help.data).unwrap();
        assert_eq!(data["command"], "help");
        assert_eq!(
            (&data["args"][0]["name"], &data["args"][0]["required"]),
            (&serde_json::json!("command"), &serde_json::json!(false))
        );
        assert!(!help.text.contains('\u{1b}'));

        let argv: Vec<String> = ["omaddons", "help", "hepl"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        let err = for_command(Some("hepl"), &argv).unwrap_err();
        assert_eq!(err.code, ErrorCode::Usage);
        assert_eq!(err.fix.argv().unwrap(), ["omaddons", "help", "help"]);
    }

    /// `auth github` is a subcommand of `auth`; a value of the next positional (`auth
    /// github set`) stays on the command, and no message ends in an empty list.
    #[test]
    fn positional_values_resolve_to_their_command() {
        for name in ["auth github", "auth github set", "auth github status"] {
            let help = for_command(Some(name), &[]).unwrap();
            let data = serde_json::to_value(&help.data).unwrap();
            assert_eq!(data["command"], "auth github", "{name}");
            let usage = data["usage"].as_str().unwrap();
            assert!(
                usage.contains("auth github") && usage.contains("[ACTION]"),
                "{name}: {data}"
            );
        }
        let auth = for_command(Some("auth"), &[]).unwrap();
        let data = serde_json::to_value(&auth.data).unwrap();
        assert_eq!(data["command"], "auth");
        assert!(auth.text.contains("github"), "{}", auth.text);
        let argv = |words: &[&str]| -> Vec<String> {
            ["omaddons", "help"]
                .iter()
                .chain(words)
                .map(|s| (*s).to_owned())
                .collect()
        };
        let err = for_command(Some("auth gihub"), &argv(&["auth", "gihub"])).unwrap_err();
        assert_eq!(err.code, ErrorCode::Usage);
        assert_eq!(
            err.message,
            "there is no command \"auth gihub\": auth has github"
        );
        assert_eq!(
            err.fix.argv().unwrap(),
            ["omaddons", "help", "auth", "github"]
        );
        let err =
            for_command(Some("auth github sett"), &argv(&["auth", "github", "sett"])).unwrap_err();
        assert_eq!(
            err.message,
            "there is no command \"auth github sett\": auth github takes ACTION (status, set, clear)"
        );
        assert_eq!(
            err.fix.argv().unwrap(),
            ["omaddons", "help", "auth", "github", "set"]
        );
        let err = for_command(Some("auth github set now"), &[]).unwrap_err();
        assert_eq!(
            err.message,
            "there is no command \"auth github set now\": auth github set takes no subcommands"
        );
        // A free-form positional is no list of values.
        let err = for_command(Some("install gh:o/foo"), &[]).unwrap_err();
        assert_eq!(
            err.message,
            "there is no command \"install gh:o/foo\": install takes no subcommands"
        );
        assert_eq!(err.fix.argv().unwrap(), ["omaddons", "help", "install"]);
    }

    fn words(line: &str) -> Vec<String> {
        line.split(' ').map(str::to_owned).collect()
    }

    #[test]
    fn the_depth_follows_the_flag_typed() {
        for (line, depth) in [
            ("omaddons status -h", Depth::Short),
            ("omaddons status --help", Depth::Long),
            ("omaddons -h status --help", Depth::Long),
            ("omaddons help status", Depth::Short),
            ("omaddons install -- --help", Depth::Short),
        ] {
            assert_eq!(Depth::of(&words(line)), depth, "{line}");
        }
        // `help <command>` is always the whole page, as `<command> --help` is.
        let via_help = for_command(Some("status"), &words("omaddons help status")).unwrap();
        let flag = for_flag(Some("status"), &words("omaddons status --help")).unwrap();
        let short = for_flag(Some("status"), &words("omaddons status -h")).unwrap();
        assert_eq!(via_help.text, flag.text);
        assert_ne!(short.text, flag.text, "-h is the compact page");
    }

    /// The render-only copy lists `--if-match` exactly on the commands that bind, never
    /// `--robot`, and leaves the tree `help --json` reads as it was.
    #[test]
    fn the_page_lists_if_match_only_where_the_command_binds() {
        for row in super::super::COMMANDS {
            let long = for_command(Some(row.name), &[]).unwrap();
            let binds = super::super::precondition::BINDING_COMMANDS.contains(&row.name);
            assert_eq!(
                long.text.contains("--if-match <TOKEN>"),
                binds,
                "{}: --if-match listed {}",
                row.name,
                long.text.contains("--if-match <TOKEN>")
            );
            assert!(
                !long.text.contains("--robot"),
                "{}: --robot listed",
                row.name
            );
            assert!(
                long.text.ends_with(&format!("{LEARN_MORE}\n")),
                "{}",
                row.name
            );
            let short = for_flag(Some(row.name), &words("omaddons -h")).unwrap();
            assert!(!short.text.contains("Global options:"), "{}", row.name);
            assert!(short.text.contains("\nGlobal: -t <TARGET>"), "{}", row.name);
            // `help --json` still names every global flag, `--if-match` and `--robot` too.
            let data = serde_json::to_value(&long.data).unwrap();
            let names: Vec<&str> = data["flags"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|flag| flag["name"].as_str())
                .collect();
            for name in ["if-match", "robot", "json"] {
                assert!(
                    names.contains(&name),
                    "{}: help --json lacks {name}",
                    row.name
                );
            }
        }
    }

    /// Every `-h` page has examples; no page says an enumeration twice; a command's page
    /// names its aliases; the doctor's exit table keeps its columns.
    #[test]
    fn pages_carry_examples_aliases_and_each_list_once() {
        let mut root = Cli::command();
        root.build();
        for row in super::super::COMMANDS {
            let short = for_flag(Some(row.name), &words("omaddons -h")).unwrap();
            assert!(
                short.text.contains("\nExamples:\n  "),
                "{}:\n{}",
                row.name,
                short.text
            );
            let long = for_command(Some(row.name), &[]).unwrap();
            let mut sub = &root;
            for word in row.name.split(' ') {
                sub = sub.find_subcommand(word).unwrap();
            }
            for arg in sub.get_arguments().filter(|arg| lists_its_values(arg)) {
                let values: Vec<String> = arg
                    .get_possible_values()
                    .iter()
                    .filter(|value| !value.is_hide_set())
                    .map(|value| value.get_name().to_owned())
                    .collect();
                let listed = format!("[possible values: {}]", values.join(", "));
                for page in [&short.text, &long.text] {
                    assert!(!page.contains(&listed), "{}: {listed} twice", row.name);
                }
            }
            let aliases: Vec<&str> = sub.get_visible_aliases().collect();
            if !aliases.is_empty() {
                let line = format!("\nAliases: {}\n", aliases.join(", "));
                assert!(long.text.contains(&line), "{}: {line:?}", row.name);
            }
        }
        let remove = for_command(Some("remove"), &[]).unwrap();
        assert!(
            remove.text.contains("\nAliases: uninstall, rm, delete\n"),
            "{}",
            remove.text
        );
        let doctor = for_command(Some("doctor"), &[]).unwrap();
        let table: Vec<&str> = doctor
            .text
            .split("The doctor exits with its own codes:\n")
            .nth(1)
            .unwrap()
            .lines()
            .take_while(|line| !line.is_empty())
            .collect();
        assert!(table.len() >= 11, "{table:?}");
        for line in table {
            assert!(
                line.starts_with("  "),
                "column 0 inside the table: {line:?}"
            );
        }
    }

    /// The summaries the command lists show (bare help's groups, `help --json`,
    /// `capabilities`, the guide) are one-line imperative phrases: every visible
    /// command's about is at most 60 characters, without parentheses or a trailing
    /// period, and opens with a verb, not a noun or a question; every visible flag's first
    /// help paragraph is at most 72 characters. Detail goes to later paragraphs, which
    /// only `--help` shows.
    #[test]
    fn summaries_are_one_imperative_line() {
        const ABOUT_MAX: usize = 60;
        const FLAG_MAX: usize = 72;
        /// Openers of a noun phrase or a question, never of an imperative.
        const NOT_A_VERB: &[&str] = &[
            "A",
            "An",
            "The",
            "Which",
            "What",
            "Whether",
            "Where",
            "Every",
            "Each",
            "One",
            "Documentation",
            "Triage:",
        ];
        fn walk(cmd: &clap::Command, path: &mut Vec<String>, faults: &mut Vec<String>) {
            for sub in cmd.get_subcommands().filter(|sub| !sub.is_hide_set()) {
                path.push(sub.get_name().to_owned());
                let line = path.join(" ");
                let about = sub.get_about().map(ToString::to_string).unwrap_or_default();
                let first = about.split_whitespace().next().unwrap_or_default();
                if about.is_empty()
                    || about.chars().count() > ABOUT_MAX
                    || about.contains(['(', ')'])
                    || about.ends_with('.')
                    || about.contains('\n')
                    || NOT_A_VERB.contains(&first)
                {
                    faults.push(format!(
                        "{line}: about {:?} ({} chars)",
                        about,
                        about.chars().count()
                    ));
                }
                for arg in sub
                    .get_arguments()
                    .filter(|arg| !arg.is_hide_set() && !arg.is_global_set())
                {
                    let help = arg.get_help().map(ToString::to_string).unwrap_or_default();
                    if help.chars().count() > FLAG_MAX {
                        faults.push(format!(
                            "{line} {}: help {help:?} ({} chars)",
                            arg.get_id(),
                            help.chars().count()
                        ));
                    }
                }
                walk(sub, path, faults);
                path.pop();
            }
        }
        let mut root = Cli::command();
        root.build();
        let mut faults = Vec::new();
        for arg in root.get_arguments().filter(|arg| !arg.is_hide_set()) {
            let help = arg.get_help().map(ToString::to_string).unwrap_or_default();
            if help.chars().count() > FLAG_MAX {
                faults.push(format!(
                    "global {}: help {help:?} ({} chars)",
                    arg.get_id(),
                    help.chars().count()
                ));
            }
        }
        walk(&root, &mut Vec::new(), &mut faults);
        assert!(
            faults.is_empty(),
            "{} summaries break the rules:\n{}",
            faults.len(),
            faults.join("\n")
        );
    }

    /// The full top-level page names every top-level command once, with its aliases and
    /// summary, every global option, every variable the binary reads and every exit code,
    /// within 100 columns; bare help stays concise and points at it; both carry the same
    /// JSON.
    #[test]
    fn the_full_page_names_every_command_option_and_variable() {
        use crate::contract::env::{ENV_VARS, EnvKind};
        let full = full();
        let concise = bare();
        assert_eq!(
            for_command(None, &[]).unwrap().text,
            full.text,
            "omaddons help is the full page"
        );
        assert_eq!(
            for_flag(None, &words("omaddons --help")).unwrap().text,
            full.text
        );
        assert_eq!(
            for_flag(None, &words("omaddons -h")).unwrap().text,
            concise.text
        );
        assert!(
            concise.text.ends_with(&format!("\n{FULL_POINTER}\n")),
            "{}",
            concise.text
        );
        assert!(concise.text.lines().count() < 30, "{}", concise.text);
        assert_eq!(
            serde_json::to_string(&full.data).unwrap(),
            serde_json::to_string(&concise.data).unwrap(),
            "help --json is the same either way"
        );
        let root = Cli::command();
        for sub in root.get_subcommands().filter(|sub| !sub.is_hide_set()) {
            let about = sub.get_about().map(ToString::to_string).unwrap_or_default();
            let listed: Vec<&str> = full
                .text
                .lines()
                .filter(|line| {
                    line.starts_with(&format!("  {}", sub.get_name())) && line.contains(&about)
                })
                .collect();
            assert_eq!(
                listed.len(),
                1,
                "{} once, with its about: {listed:?}",
                sub.get_name()
            );
            for alias in sub.get_visible_aliases() {
                assert!(
                    listed[0].contains(alias),
                    "{}'s alias {alias}",
                    sub.get_name()
                );
            }
        }
        for row in super::super::COMMANDS
            .iter()
            .filter(|row| !row.name.contains(' '))
        {
            assert!(
                full.text.contains(&format!("\n  {}", row.name)),
                "{}",
                row.name
            );
        }
        for arg in root.get_arguments().filter(|arg| !arg.is_hide_set()) {
            let long = arg.get_long().unwrap_or_default();
            assert!(
                [" ", "\n", "..."]
                    .iter()
                    .any(|after| full.text.contains(&format!("--{long}{after}"))),
                "--{long} listed"
            );
        }
        for var in ENV_VARS {
            let listed = full.text.contains(var.name);
            assert_eq!(
                listed,
                var.kind != EnvKind::Harness,
                "{} ({:?}) listed: {listed}",
                var.name,
                var.kind
            );
        }
        for exit in Exit::ALL {
            assert!(full.text.contains(exit.name()), "{}", exit.name());
        }
        assert!(
            full.text
                .lines()
                .all(|line| line.chars().count() <= HELP_MAX_COLUMNS)
        );
    }

    /// Every flag `-h`'s summary names is a global option of the tree.
    #[test]
    fn the_global_summary_names_real_options() {
        let mut root = Cli::command();
        root.build();
        let spelled: Vec<String> = root
            .get_arguments()
            .filter(|arg| arg.is_global_set())
            .flat_map(|arg| {
                let mut names: Vec<String> = arg
                    .get_short()
                    .map(|s| format!("-{s}"))
                    .into_iter()
                    .collect();
                names.extend(arg.get_long().map(|l| format!("--{l}")));
                names
            })
            .collect();
        for item in GLOBAL_SUMMARY {
            let flag = item.split(' ').next().unwrap_or_default();
            assert!(
                spelled.iter().any(|s| s == flag),
                "{item}: no such global option"
            );
        }
    }

    #[test]
    fn an_enumeration_is_said_once() {
        let arg = |help: &str| {
            clap::Arg::new("channel")
                .long("channel")
                .value_parser(["stable", "beta", "alpha"])
                .help(help.to_owned())
        };
        for (help, spelled_out) in [
            ("stable, beta or alpha (default: stable)", true),
            ("Release channel", false),
            ("stable or beta", false),
            ("a stable-ish beta, or alpha", false),
        ] {
            assert_eq!(lists_its_values(&arg(help)), spelled_out, "{help:?}");
        }
        assert!(!lists_its_values(
            &clap::Arg::new("deep")
                .long("deep")
                .action(clap::ArgAction::SetTrue)
                .help("deep")
        ));
    }

    #[test]
    fn examples_align_their_descriptions() {
        let block = examples_block(&[
            ("omaddons status", "every client"),
            ("omaddons status --cached", "no scan"),
            (
                "omaddons doctor --only fm-userland_state-target-vanished",
                "one check",
            ),
            ("omaddons config path", ""),
        ]);
        assert_eq!(
            block,
            "Examples:\n  omaddons status           every client\n  omaddons status --cached  no scan\n  \
             omaddons doctor --only fm-userland_state-target-vanished\n                            one check\n  \
             omaddons config path"
        );
    }

    /// `fit` wraps every line to the width it is given: a table row or an example hangs at
    /// its last column, prose at its own indent, a column too deep four past the indent.
    #[test]
    fn fit_hangs_each_line_at_its_last_column() {
        let words = "word ".repeat(20);
        let row = format!("  64  usage_error  {}", words.trim_end());
        let prose = format!("  {}", words.trim_end());
        for width in [40, 60, 80, 100] {
            for (case, line, hang) in [
                ("a table row", row.as_str(), 19),
                ("prose", prose.as_str(), 2),
            ] {
                let fitted = fit(line, width);
                let lines: Vec<&str> = fitted.lines().collect();
                assert!(
                    lines.len() > 1 || line.chars().count() <= width,
                    "{case} at {width}"
                );
                for (at, piece) in lines.iter().enumerate() {
                    assert!(
                        piece.chars().count() <= width,
                        "{case} at {width}: {piece:?}"
                    );
                    if at > 0 {
                        assert!(
                            piece.starts_with(&" ".repeat(hang)) && !piece[hang..].starts_with(' '),
                            "{case} at {width}: {piece:?} hangs at {hang}"
                        );
                    }
                }
                assert_eq!(
                    prose_words(&fitted),
                    prose_words(line),
                    "{case}: words kept"
                );
            }
        }
        // A column too deep for the width hangs four past the indent; a short line stays.
        let deep = format!("  {:<50}  {}", "omaddons doctor", words.trim_end());
        let fitted = fit(&deep, 60);
        assert!(
            fitted
                .lines()
                .skip(1)
                .all(|piece| piece.starts_with("      w")),
            "{fitted}"
        );
        assert_eq!(fit("  short  line", 60), "  short  line");
        assert_eq!(last_column("  0   healthy      fine"), Some(19));
        assert_eq!(last_column("plain prose here"), None);
        assert_eq!(last_column("trailing gap  "), None);
    }

    /// The page is styled only where the output policy allows color; its JSON never is,
    /// and the plain page holds no escape at all.
    #[test]
    fn color_follows_the_output_policy() {
        let render = |outcome: &HelpOutcome, color: bool| {
            let style = Style {
                color,
                verbose: 0,
                quiet: false,
            };
            let mut out = Vec::new();
            outcome.render_human(&mut out, &style).unwrap();
            String::from_utf8(out).unwrap()
        };
        for outcome in [
            for_flag(Some("status"), &words("omaddons status -h")).unwrap(),
            for_command(Some("install"), &[]).unwrap(),
            bare(),
        ] {
            let colored = render(&outcome, true);
            let plain = render(&outcome, false);
            assert!(
                colored.contains("\u{1b}[1m"),
                "bold when allowed:\n{colored}"
            );
            assert!(!plain.contains('\u{1b}'), "plain otherwise:\n{plain}");
            let json = serde_json::to_string(&outcome.data).unwrap();
            assert!(!json.contains("\\u001b"), "help --json never: {json}");
        }
        let colored = render(&for_command(Some("status"), &[]).unwrap(), true);
        for heading in ["Usage:", "Options:", "Global options:", "--cached"] {
            assert!(
                colored.contains(&format!("{heading}\u{1b}[0m")),
                "{heading} styled:\n{colored}"
            );
        }
    }

    /// The width a page is wrapped at is clap's (the test's terminal or 100), capped at
    /// [`HELP_MAX_COLUMNS`].
    #[test]
    fn the_width_is_clap_s_capped_at_100() {
        let width = help_width();
        assert!((1..=HELP_MAX_COLUMNS).contains(&width), "{width}");
    }
}
