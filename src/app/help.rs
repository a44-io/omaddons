//! `omaddons`, `omaddons help [<command>]`, `-h` and `-V`. Help is data: generated
//! from the live clap tree and the exit dictionary, so it never advertises a command
//! this build does not have, and it is concise enough for an agent's first contact.

use std::io::Write;

use clap::CommandFactory;
use schemars::JsonSchema;
use serde::Serialize;

use super::{Group, group_of};
use crate::cli::Cli;
use crate::error::{Error, ErrorCode, Exit, Suggestion};
use crate::output::{Outcome, Style, table};

/// The column the command names of a group line start at.
const GROUP_COLUMN: usize = 10;
/// Bare help's lines stay narrower than this.
const MAX_WIDTH: usize = 80;

/// Help or version, as data and as text.
#[derive(Debug)]
pub struct HelpOutcome {
    command: &'static str,
    data: HelpData,
    text: String,
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

    fn render_human(&self, w: &mut dyn Write, _style: &Style) -> std::io::Result<()> {
        w.write_all(self.text.as_bytes())
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
fn render_groups(commands: &[(&str, Group)]) -> String {
    let mut text = String::new();
    for group in Group::ALL {
        let names: Vec<&str> = commands
            .iter()
            .filter(|(_, g)| *g == group)
            .map(|(name, _)| *name)
            .collect();
        let Some((first, rest)) = names.split_first() else {
            continue;
        };
        let mut line = format!("{:<GROUP_COLUMN$}{first}", group.label());
        for name in rest {
            if line.len() + 2 + name.len() >= MAX_WIDTH {
                text.push_str(&line);
                text.push('\n');
                line = format!("{:GROUP_COLUMN$}{name}", "");
            } else {
                line.push_str("  ");
                line.push_str(name);
            }
        }
        text.push_str(&line);
        text.push('\n');
    }
    text
}

/// Bare help's text for `commands` (name and group, in display order).
fn bare_text(usage: &str, commands: &[(&str, Group)]) -> String {
    let mut text = String::new();
    text.push_str("World of Warcraft addon manager for Linux and an Omarchy widget\n\n");
    text.push_str(&format!("Usage: {usage}\n\n"));
    text.push_str(&render_groups(commands));
    text.push('\n');
    if commands.iter().any(|(name, _)| *name == "status") {
        text.push_str("Start with `omaddons status`. ");
    }
    text.push_str("Run `omaddons help <command>` for details; add --json for one JSON envelope on stdout.\n\nExit codes:\n");
    text.push_str(&exit_table());
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
    let text = bare_text(&usage, &listed);

    HelpOutcome {
        command: "help",
        data: HelpData::Bare(BareHelp {
            name: "omaddons".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            usage,
            commands,
            global_flags: flags(&cmd),
            exit_codes,
        }),
        text,
    }
}

/// `omaddons help [<command>]`; an unknown name is a `usage` error suggesting the
/// closest command.
pub fn for_command(name: Option<&str>, argv: &[String]) -> Result<HelpOutcome, Error> {
    let Some(name) = name else { return Ok(bare()) };
    let mut root = Cli::command();
    root.build();
    // A nested path such as `targets add` or `auth github` walks one level per word. A
    // word that is a possible value of the command's next positional (`auth github set`)
    // stays on that command, and so does every word after it. An unknown word is
    // suggested among the subcommands and values the command it follows takes.
    let mut sub = &mut root;
    let mut walked: Vec<&str> = Vec::new();
    let mut values = 0;
    for part in name.split_whitespace() {
        if values == 0 && sub.find_subcommand(part).is_some() {
            walked.push(part);
            let Some(next) = sub.find_subcommand_mut(part) else {
                return Err(Error::internal(format!(
                    "{part} vanished from the command tree"
                )));
            };
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
    let text = sub.render_long_help().to_string();
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
            command: sub.get_name().to_owned(),
            about: sub.get_about().map(ToString::to_string).unwrap_or_default(),
            usage,
            args,
            flags: flags(sub),
        }),
        text,
    })
}

/// `omaddons --version`.
pub fn version() -> HelpOutcome {
    let version = env!("CARGO_PKG_VERSION").to_owned();
    HelpOutcome {
        command: "version",
        text: format!("omaddons {version}\n"),
        data: HelpData::Version(VersionInfo { version }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn groups_render_one_line_each_and_wrap_under_the_first_name() {
        let commands = [
            ("status", Group::Look),
            ("list", Group::Look),
            ("help", Group::Setup),
        ];
        assert_eq!(
            render_groups(&commands),
            "Look      status  list\nSetup     help\n",
            "a group without commands is left out"
        );
        let long: Vec<(String, Group)> = (0..12)
            .map(|i| (format!("command{i:02}"), Group::Change))
            .collect();
        let long: Vec<(&str, Group)> = long.iter().map(|(n, g)| (n.as_str(), *g)).collect();
        let text = render_groups(&long);
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
        let text = bare_text("omaddons [OPTIONS] <COMMAND>", &planned);
        assert!(
            text.lines().count() < 30,
            "{} lines\n{text}",
            text.lines().count()
        );
        let groups = render_groups(&planned);
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
            assert_eq!(data["command"], "github", "{name}");
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
}
