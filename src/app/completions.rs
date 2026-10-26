//! `omaddons completions <SHELL>`: a static completion script for one shell, generated in
//! memory from the live clap tree (M4-D16). It reads nothing and writes nothing.

use std::io::Write;

use clap::CommandFactory;
use clap_complete::Generator as _;
use schemars::JsonSchema;
use serde::Serialize;

use crate::cli::Cli;
use crate::error::Error;
use crate::output::{Outcome, Style};

/// The terminal width the script is generated at, so its bytes never depend on the
/// terminal it is printed to (M4-D18). A presentation constant, like help's `MAX_WIDTH`.
const SCRIPT_TERM_WIDTH: usize = 100;

/// A shell `completions` writes a script for: clap_complete's five, by their wire names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Shell {
    Bash,
    Zsh,
    Fish,
    Elvish,
    Powershell,
}

impl Shell {
    /// In help's order.
    pub const ALL: [Self; 5] = [
        Self::Bash,
        Self::Zsh,
        Self::Fish,
        Self::Elvish,
        Self::Powershell,
    ];

    /// clap_complete's generator for this shell.
    const fn generator(self) -> clap_complete::Shell {
        match self {
            Self::Bash => clap_complete::Shell::Bash,
            Self::Zsh => clap_complete::Shell::Zsh,
            Self::Fish => clap_complete::Shell::Fish,
            Self::Elvish => clap_complete::Shell::Elvish,
            Self::Powershell => clap_complete::Shell::PowerShell,
        }
    }
}

/// `completions`' arguments.
#[derive(Debug, Clone, Copy, clap::Args)]
pub struct CompletionsArgs {
    /// The shell the script is for
    #[arg(value_enum, value_name = "SHELL")]
    pub shell: Shell,
}

/// The `data` of a `completions` envelope.
#[derive(Debug, Serialize, JsonSchema)]
pub struct CompletionsData {
    /// The shell the script is for.
    pub shell: Shell,
    /// The whole script: exactly the bytes the human form prints.
    pub script: String,
}

/// What `completions` returns.
#[derive(Debug)]
pub struct CompletionsOutcome {
    data: CompletionsData,
}

impl Outcome for CompletionsOutcome {
    type Data = CompletionsData;

    fn command(&self) -> &'static str {
        "completions"
    }

    fn data(&self) -> &CompletionsData {
        &self.data
    }

    /// The script raw, nothing before or after it, so `> file` saves exactly the script.
    fn render_human(&self, w: &mut dyn Write, _style: &Style) -> std::io::Result<()> {
        w.write_all(self.data.script.as_bytes())
    }
}

/// `omaddons completions <SHELL>`.
pub fn run(args: &CompletionsArgs) -> crate::error::Result<CompletionsOutcome> {
    let script = script(args.shell)?;
    Ok(CompletionsOutcome {
        data: CompletionsData {
            shell: args.shell,
            script,
        },
    })
}

/// The script for `shell`, generated from `Cli::command()` at a fixed width with the bin
/// name `omaddons`: exactly what `clap_complete::generate` does, with its write error
/// returned instead of a panic inside the dependency.
pub fn script(shell: Shell) -> crate::error::Result<String> {
    let mut cmd = Cli::command()
        .term_width(SCRIPT_TERM_WIDTH)
        .max_term_width(SCRIPT_TERM_WIDTH);
    cmd.set_bin_name("omaddons");
    cmd.build();
    let mut buf = Vec::new();
    let generator = shell.generator();
    generator.try_generate(&cmd, &mut buf).map_err(|e| {
        Error::internal(format!(
            "the {generator} completion script could not be generated: {e}"
        ))
    })?;
    String::from_utf8(buf).map_err(|e| {
        Error::internal(format!(
            "the {generator} completion script is not UTF-8: {e}"
        ))
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use clap::ValueEnum;

    use super::*;

    fn clap_value(shell: Shell) -> String {
        shell
            .to_possible_value()
            .map(|v| v.get_name().to_owned())
            .unwrap_or_default()
    }

    /// The script for `shell`, failing the test on an error.
    fn generated(shell: Shell) -> String {
        let script = script(shell);
        assert!(script.is_ok(), "{shell:?}: {script:?}");
        script.unwrap_or_default()
    }

    fn head(script: &str) -> String {
        script.chars().take(200).collect()
    }

    #[test]
    fn shell_values_are_clap_completes_five() {
        let ours: BTreeSet<String> = Shell::ALL.into_iter().map(clap_value).collect();
        let theirs: BTreeSet<String> = clap_complete::Shell::value_variants()
            .iter()
            .filter_map(|s| s.to_possible_value())
            .map(|v| v.get_name().to_owned())
            .collect();
        assert_eq!(ours, theirs);
        assert_eq!(ours.len(), 5);
    }

    #[test]
    fn shell_wire_names_match_their_clap_values() {
        for (case, shell, name) in [
            ("bash_is_bash", Shell::Bash, "bash"),
            ("zsh_is_zsh", Shell::Zsh, "zsh"),
            ("fish_is_fish", Shell::Fish, "fish"),
            ("elvish_is_elvish", Shell::Elvish, "elvish"),
            ("powershell_is_powershell", Shell::Powershell, "powershell"),
        ] {
            let wire = serde_json::to_value(shell).ok();
            assert_eq!(wire, Some(serde_json::json!(name)), "{case}: serde");
            assert_eq!(clap_value(shell), name, "{case}: clap");
            assert_eq!(shell.generator().to_string(), name, "{case}: clap_complete");
        }
    }

    #[test]
    fn shell_order_is_helps() {
        assert_eq!(Shell::value_variants(), Shell::ALL);
        let names: Vec<String> = Shell::ALL.into_iter().map(clap_value).collect();
        assert_eq!(names, ["bash", "zsh", "fish", "elvish", "powershell"]);
    }

    #[test]
    fn each_shell_generates_a_script_that_registers_omaddons() {
        for (shell, marker, at_start) in [
            (Shell::Bash, "_omaddons() {", true),
            (Shell::Bash, "complete -F _omaddons", false),
            (Shell::Zsh, "#compdef omaddons", true),
            (Shell::Fish, "complete -c omaddons", false),
            (
                Shell::Elvish,
                "edit:completion:arg-completer[omaddons]",
                false,
            ),
            (
                Shell::Powershell,
                "Register-ArgumentCompleter -Native -CommandName 'omaddons'",
                false,
            ),
        ] {
            let script = generated(shell);
            let found = if at_start {
                script.starts_with(marker)
            } else {
                script.contains(marker)
            };
            assert!(
                found,
                "{shell:?}: expected {marker:?} in a {}-byte script starting {:?}",
                script.len(),
                head(&script)
            );
            assert!(
                script.ends_with('\n'),
                "{shell:?}: a {}-byte script that does not end with a newline, starting {:?}",
                script.len(),
                head(&script)
            );
        }
    }

    #[test]
    fn generation_is_deterministic() {
        for shell in Shell::ALL {
            let (a, b) = (generated(shell), generated(shell));
            assert!(
                a == b,
                "{shell:?}: two runs differ ({} and {} bytes), starting {:?}",
                a.len(),
                b.len(),
                head(&a)
            );
        }
    }

    #[test]
    fn the_outcome_is_the_script_raw() {
        let outcome = run(&CompletionsArgs { shell: Shell::Bash });
        assert!(outcome.is_ok(), "{outcome:?}");
        let Ok(outcome) = outcome else { return };
        assert_eq!(outcome.command(), "completions");
        let mut out = Vec::new();
        let rendered = outcome.render_human(&mut out, &Style::default());
        assert!(rendered.is_ok(), "{rendered:?}");
        assert!(
            out == outcome.data().script.as_bytes(),
            "the human form is not the script: {} and {} bytes",
            out.len(),
            outcome.data().script.len()
        );
        let json = serde_json::to_string(outcome.data()).unwrap_or_default();
        assert!(
            json.starts_with(r#"{"shell":"bash","script":"_omaddons() {"#),
            "key order: {:?}",
            head(&json)
        );
    }
}
