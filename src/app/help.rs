//! `omaddons`, `omaddons help [<command>]`, `-h` and `-V`. Help is data: generated
//! from the live clap tree and the exit dictionary, so it never advertises a command
//! this build does not have, and it is concise enough for an agent's first contact.

use std::io::Write;

use clap::CommandFactory;
use schemars::JsonSchema;
use serde::Serialize;

use crate::cli::Cli;
use crate::error::{Error, ErrorCode, Exit, Suggestion};
use crate::output::{Outcome, Style, table};

/// The most common verbs, listed first in this order when the build has them.
const COMMON_VERBS: &[&str] = &[
    "status", "install", "update", "remove", "undo", "list", "targets", "info", "history", "config",
];

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

/// Bare `omaddons`: what it is, the verbs, and the exit codes.
pub fn bare() -> HelpOutcome {
    let cmd = Cli::command();
    let mut commands: Vec<CommandInfo> = cmd
        .get_subcommands()
        .map(|c| CommandInfo {
            name: c.get_name().to_owned(),
            about: c.get_about().map(ToString::to_string).unwrap_or_default(),
            aliases: c.get_visible_aliases().map(str::to_owned).collect(),
        })
        .collect();
    let rank = |name: &str| {
        COMMON_VERBS
            .iter()
            .position(|v| *v == name)
            .unwrap_or(COMMON_VERBS.len())
    };
    commands.sort_by(|a, b| (rank(&a.name), &a.name).cmp(&(rank(&b.name), &b.name)));
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

    let mut text = String::new();
    text.push_str(
        "omaddons: manage World of Warcraft addons for every client on this machine.\n\n",
    );
    text.push_str(&format!("Usage: {usage}\n\nCommands:\n"));
    let rows: Vec<Vec<String>> = commands
        .iter()
        .map(|c| vec![format!("  {}", c.name), c.about.clone()])
        .collect();
    text.push_str(&table(&rows));
    text.push('\n');
    if commands.iter().any(|c| c.name == "status") {
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
    let names: Vec<String> = root
        .get_subcommands()
        .map(|c| c.get_name().to_owned())
        .collect();
    // A nested path such as `targets add` walks one level per word.
    let found = name
        .split_whitespace()
        .try_fold(&mut root, |cmd, part| cmd.find_subcommand_mut(part));
    let Some(sub) = found else {
        let (fix, alternatives) = crate::cli::suggest(argv, name, &names)
            .unwrap_or_else(|| (Suggestion::command(["omaddons", "help"]), Vec::new()));
        let mut error = Error::new(
            ErrorCode::Usage,
            format!("there is no command {name:?}"),
            fix,
        );
        error.alternatives = alternatives;
        return Err(error);
    };
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
