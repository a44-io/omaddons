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

/// A short meaning per exit code for the human table; the JSON carries the full one.
fn short_meaning(exit: Exit) -> &'static str {
    match exit {
        Exit::Ok => "Success, including partial success (partial: true)",
        Exit::Failed => "Runtime failure; see error.retryable",
        Exit::Usage => "Unknown command or flag, bad argument, bad ref syntax",
        Exit::NotFound => "Target, ref, addon, plan or transaction not found, or ambiguous",
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
    text.push_str("World of Warcraft addon manager for Linux\n\n");
    text.push_str(&format!("Usage: {usage}\n\n"));
    text.push_str(&render_groups(commands));
    text.push('\n');
    if commands.iter().any(|(name, _)| *name == "status") {
        text.push_str("Start with `omaddons status`. ");
    }
    text.push_str("Run `omaddons help <command>` for details; add --json for one JSON envelope on stdout.\n\nExit codes:\n");
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
    text.push_str(&table(&rows));
    text
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
    // A nested path such as `targets add` walks one level per word; an unknown word is
    // suggested among the subcommands of the command it follows.
    let mut sub = &mut root;
    let mut walked: Vec<&str> = Vec::new();
    for part in name.split_whitespace() {
        if sub.find_subcommand(part).is_none() {
            let names: Vec<String> = sub
                .get_subcommands()
                .map(|c| c.get_name().to_owned())
                .collect();
            // Past the first word: the parent's help lists its subcommands.
            let parent = || {
                let mut argv = vec!["omaddons", "help"];
                argv.extend(&walked);
                (Suggestion::command(argv), Vec::new())
            };
            let (fix, alternatives) =
                crate::cli::suggest(argv, part, &names).unwrap_or_else(parent);
            let message = if walked.is_empty() {
                format!("there is no command {name:?}")
            } else {
                format!(
                    "there is no command {name:?}: {} has {}",
                    walked.join(" "),
                    names.join(", ")
                )
            };
            let mut error = Error::new(ErrorCode::Usage, message, fix);
            error.alternatives = alternatives;
            return Err(error);
        }
        walked.push(part);
        let Some(next) = sub.find_subcommand_mut(part) else {
            return Err(Error::internal(format!(
                "{part} vanished from the command tree"
            )));
        };
        sub = next;
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
        // Every command through M3's backup, in the planned layout; test-only, never
        // live help.
        let planned = [
            ("status", Look),
            ("list", Look),
            ("info", Look),
            ("outdated", Look),
            ("search", Look),
            ("history", Look),
            ("net-log", Look),
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
            ("backup", Change),
            ("targets", Setup),
            ("config", Setup),
            ("auth", Setup),
            ("help", Setup),
        ];
        assert_eq!(planned.len(), 22);
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
                "Change    install  update  remove  pin  unpin  adopt  sync  copy  undo  backup"
            ),
            "{groups}"
        );
    }

    #[test]
    fn every_subcommand_has_exactly_one_registry_entry() {
        let cmd = Cli::command();
        let clap: Vec<&str> = cmd.get_subcommands().map(clap::Command::get_name).collect();
        for name in &clap {
            let entries = super::super::COMMANDS
                .iter()
                .filter(|c| c.name == *name)
                .count();
            assert_eq!(entries, 1, "{name} needs one COMMANDS entry");
        }
        for entry in super::super::COMMANDS {
            let top = entry.name.split(' ').next().unwrap_or_default();
            assert!(clap.contains(&top), "{} is not a subcommand", entry.name);
            if top != entry.name {
                assert_eq!(
                    group_of(top).map(|(g, _)| g),
                    Some(entry.group),
                    "{} takes its parent's group",
                    entry.name
                );
            }
        }
        for (name, canonical) in [("targets add", "targets add"), ("nosuch", "omaddons")] {
            assert_eq!(super::super::canonical(name), canonical);
        }
        let wire: Vec<String> = Group::ALL
            .iter()
            .map(|g| {
                serde_json::to_value(g)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect();
        assert_eq!(wire, ["look", "change", "setup"]);
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
}
