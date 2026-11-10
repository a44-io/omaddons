//! `omaddons completions <SHELL>`: a completion script for one shell, generated in memory
//! from the live clap tree (M4-D16). It reads nothing and writes nothing.
//!
//! Every script is clap_complete's static one: commands, subcommands, aliases, flags and
//! fixed values. For bash, zsh and fish, [`glue`] adds the values omaddons' own files
//! hold (client ids, addon names and the various ids), which the script asks
//! `omaddons __complete` for at the press of Tab (`app::complete`).

mod glue;

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

/// The script for `shell`: [`static_script`], with the dynamic values' [`glue`] added for
/// bash and fish (after it) and zsh (woven in by [`weave_zsh`]).
pub fn script(shell: Shell) -> crate::error::Result<String> {
    let cmd = command();
    let mut script = static_script(shell, &cmd)?;
    let dynamic = |glue_shell| {
        glue::glue(glue_shell, &cmd).map_err(|e| {
            Error::internal(format!(
                "the {} completion script's values could not be placed: {e}",
                shell.generator()
            ))
        })
    };
    match shell {
        Shell::Bash => script.push_str(&dynamic(glue::Shell::Bash)?),
        Shell::Fish => script.push_str(&dynamic(glue::Shell::Fish)?),
        Shell::Zsh => script = weave_zsh(&script, &dynamic(glue::Shell::Zsh)?)?,
        Shell::Elvish | Shell::Powershell => {}
    }
    Ok(script)
}

/// The built command tree scripts are generated from, at a fixed width with the bin name
/// `omaddons`.
fn command() -> clap::Command {
    let mut cmd = Cli::command()
        .term_width(SCRIPT_TERM_WIDTH)
        .max_term_width(SCRIPT_TERM_WIDTH);
    cmd.set_bin_name("omaddons");
    cmd.build();
    cmd
}

/// clap_complete's script for `shell` from `cmd`: exactly what `clap_complete::generate`
/// does, with its write error returned instead of a panic inside the dependency.
fn static_script(shell: Shell, cmd: &clap::Command) -> crate::error::Result<String> {
    let mut buf = Vec::new();
    let generator = shell.generator();
    generator.try_generate(cmd, &mut buf).map_err(|e| {
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

/// clap's zsh function, as its script defines it.
const ZSH_DEFINITION: &str = "\n_omaddons() {\n";
/// The same function under the name the glue falls back to.
const ZSH_RENAMED: &str = "\n_omaddons_static() {\n";
/// The script's last lines: run `_omaddons` when autoloaded, else register it.
const ZSH_TRAILER: &str = "\nif [ \"$funcstack[1]\" = \"_omaddons\" ]; then\n";

/// clap's zsh `script` with its `_omaddons` renamed `_omaddons_static` and `glue` (which
/// defines the new `_omaddons`) inserted before the last lines, which then call or
/// register the new one: the file's `#compdef` line and its autoload work as before.
fn weave_zsh(script: &str, glue: &str) -> crate::error::Result<String> {
    let found = |what: &str| script.matches(what).count();
    if found(ZSH_DEFINITION) != 1 || found(ZSH_TRAILER) != 1 {
        return Err(Error::internal(format!(
            "the zsh completion script has {} definitions of _omaddons and {} trailers, not one each",
            found(ZSH_DEFINITION),
            found(ZSH_TRAILER)
        )));
    }
    let renamed = script.replacen(ZSH_DEFINITION, ZSH_RENAMED, 1);
    let at = renamed.find(ZSH_TRAILER).map_or(renamed.len(), |at| at + 1);
    let (head, tail) = renamed.split_at(at);
    Ok(format!("{head}{glue}{tail}"))
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
    fn every_script_is_claps_static_one_with_the_glue_added() {
        let cmd = command();
        for shell in Shell::ALL {
            let full = generated(shell);
            let base = static_script(shell, &cmd).unwrap_or_default();
            assert!(!base.is_empty(), "{shell:?}");
            match shell {
                Shell::Bash | Shell::Fish => {
                    assert!(full.starts_with(&base), "{shell:?}: clap's script first");
                    assert!(full.len() > base.len(), "{shell:?}: then the glue");
                    assert!(full.contains("omaddons __complete "), "{shell:?}");
                }
                Shell::Zsh => {
                    let glue = glue::glue(glue::Shell::Zsh, &cmd).unwrap_or_default();
                    let unwoven =
                        full.replacen(&glue, "", 1)
                            .replacen(ZSH_RENAMED, ZSH_DEFINITION, 1);
                    assert!(unwoven == base, "zsh: clap's script, one function renamed");
                    let trailer = full.find(ZSH_TRAILER).unwrap_or_default();
                    let static_fn = full.find(ZSH_RENAMED).unwrap_or(usize::MAX);
                    let dynamic_fn = full.find(ZSH_DEFINITION).unwrap_or(usize::MAX);
                    assert!(static_fn < dynamic_fn && dynamic_fn < trailer, "zsh order");
                    assert!(full.starts_with("#compdef omaddons\n"));
                }
                Shell::Elvish | Shell::Powershell => {
                    assert!(full == base, "{shell:?} stays static");
                }
            }
        }
    }

    #[test]
    fn the_zsh_weave_refuses_a_script_it_does_not_know() {
        let known = "#compdef omaddons\n\n_omaddons() {\n}\n\nif [ \"$funcstack[1]\" = \"_omaddons\" ]; then\n    _omaddons \"$@\"\nfi\n";
        let woven = weave_zsh(known, "GLUE\n");
        assert_eq!(
            woven.ok().as_deref(),
            Some(
                "#compdef omaddons\n\n_omaddons_static() {\n}\n\nGLUE\nif [ \"$funcstack[1]\" = \"_omaddons\" ]; then\n    _omaddons \"$@\"\nfi\n"
            )
        );
        for (case, script) in [
            (
                "no trailer",
                "#compdef omaddons\n\n_omaddons() {\n}\n".to_owned(),
            ),
            (
                "no definition",
                known.replace("_omaddons() {", "_other() {"),
            ),
            (
                "two definitions",
                known.replace("\n}\n", "\n}\n\n_omaddons() {\n}\n"),
            ),
        ] {
            assert!(weave_zsh(&script, "GLUE\n").is_err(), "{case}");
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
