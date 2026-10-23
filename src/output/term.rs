//! Terminal and environment detection, resolved once per run into an `OutputConfig`.
//! The environment comes in as a `TermEnv` value, so the truth tables are testable
//! without touching the process environment.

use std::ffi::OsString;
use std::io::IsTerminal;

/// What stdout carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputMode {
    /// Exactly one JSON envelope, on success and on failure.
    Json,
    /// Human-formatted data; empty on error.
    Human,
}

/// What stderr carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StderrMode {
    /// One JSON object per line, each with an `event`.
    Ndjson,
    /// `warning: …`, `error[<code>]: …` lines.
    Human,
}

/// How long operations report progress on stderr.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressMode {
    Json,
    Human,
    None,
}

/// Human rendering options.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Style {
    /// ANSI color on stdout.
    pub color: bool,
    /// `-v` count.
    pub verbose: u8,
    /// `--quiet`.
    pub quiet: bool,
}

/// Everything rendering needs to know about this run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputConfig {
    pub output: OutputMode,
    pub stderr: StderrMode,
    pub progress: ProgressMode,
    pub style: Style,
    /// ANSI color on stderr (human stderr mode only).
    pub stderr_color: bool,
}

impl OutputConfig {
    /// For tests and fallbacks: JSON on stdout, NDJSON on stderr, no progress, no color.
    pub fn machine() -> Self {
        Self {
            output: OutputMode::Json,
            stderr: StderrMode::Ndjson,
            progress: ProgressMode::None,
            style: Style::default(),
            stderr_color: false,
        }
    }
}

/// The parts of the process environment that affect rendering.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TermEnv {
    pub stdout_tty: bool,
    pub stderr_tty: bool,
    pub no_color: Option<String>,
    pub ci: Option<String>,
    pub term: Option<String>,
    /// `OMADDONS_FORMAT`.
    pub format: Option<String>,
}

impl TermEnv {
    pub fn from_process() -> Self {
        let var = |name: &str| std::env::var(name).ok();
        Self {
            stdout_tty: std::io::stdout().is_terminal(),
            stderr_tty: std::io::stderr().is_terminal(),
            no_color: var("NO_COLOR"),
            ci: var("CI"),
            term: var("TERM"),
            format: var("OMADDONS_FORMAT"),
        }
    }

    fn set(value: Option<&String>) -> bool {
        value.is_some_and(|v| !v.is_empty())
    }

    fn ci(&self) -> bool {
        Self::set(self.ci.as_ref())
    }

    fn dumb(&self) -> bool {
        self.term.as_deref() == Some("dumb")
    }
}

/// The output-related global flags, as parsed by the CLI.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OutputFlags {
    pub json: bool,
    pub robot: bool,
    /// `--format json|human`.
    pub format: Option<String>,
    /// `--progress json|human|none`.
    pub progress: Option<String>,
    pub no_color: bool,
    pub no_progress: bool,
    pub quiet: bool,
    pub verbose: u8,
}

/// Resolves every mode. `--format` wins over `--json`/`--robot`, flags win over
/// `OMADDONS_FORMAT`, and the default is human output.
pub fn resolve(flags: &OutputFlags, env: &TermEnv) -> OutputConfig {
    let output = output_mode(flags.format.as_deref(), flags.json || flags.robot, env);
    let stderr = if !env.stderr_tty || env.ci() || env.dumb() {
        StderrMode::Ndjson
    } else {
        StderrMode::Human
    };
    let progress = match flags.progress.as_deref() {
        Some(p) if p.eq_ignore_ascii_case("json") => ProgressMode::Json,
        Some(p) if p.eq_ignore_ascii_case("human") => ProgressMode::Human,
        Some(_) => ProgressMode::None,
        // Wherever stderr carries NDJSON (no terminal, CI, `TERM=dumb`), never a human
        // progress line's escapes between its events.
        None if flags.no_progress || output == OutputMode::Json || stderr == StderrMode::Ndjson => {
            ProgressMode::None
        }
        None => ProgressMode::Human,
    };
    let color = !(TermEnv::set(env.no_color.as_ref())
        || flags.no_color
        || output == OutputMode::Json
        || env.dumb()
        || env.ci()
        || !env.stderr_tty);
    OutputConfig {
        output,
        stderr,
        progress,
        style: Style {
            color: color && env.stdout_tty,
            verbose: flags.verbose,
            quiet: flags.quiet,
        },
        stderr_color: color && stderr == StderrMode::Human,
    }
}

fn output_mode(format: Option<&str>, json_flag: bool, env: &TermEnv) -> OutputMode {
    match format {
        Some(f) if f.eq_ignore_ascii_case("json") => return OutputMode::Json,
        Some(f) if f.eq_ignore_ascii_case("human") => return OutputMode::Human,
        _ => {}
    }
    if json_flag {
        return OutputMode::Json;
    }
    match env.format.as_deref() {
        Some(f) if f.eq_ignore_ascii_case("json") => OutputMode::Json,
        _ => OutputMode::Human,
    }
}

/// The output mode from raw argv, for when the CLI cannot parse its arguments but must
/// still answer in the right format. Arguments after `--` are ignored.
pub fn resolve_output_mode_from_argv(argv: &[OsString], env: &TermEnv) -> OutputMode {
    let flags = flags_from_argv(argv);
    output_mode(flags.format.as_deref(), flags.json || flags.robot, env)
}

/// The presentation flags in a raw argv that did not parse, read token by token up to
/// `--`, so a usage error renders as asked (`--json`, `--no-color`, `--quiet`, …).
pub fn flags_from_argv(argv: &[OsString]) -> OutputFlags {
    let mut flags = OutputFlags::default();
    let args = argv
        .iter()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned());
    // A flag's value is the next token unless that is a flag itself, which clap
    // refuses as the value too: `--format --json` still asks for JSON.
    let value =
        |args: &mut std::iter::Peekable<_>| args.next_if(|next: &String| !next.starts_with('-'));
    let mut args = args.peekable();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--" => break,
            "--json" => flags.json = true,
            "--robot" => flags.robot = true,
            "--format" => flags.format = value(&mut args),
            "--progress" => flags.progress = value(&mut args),
            "--no-color" => flags.no_color = true,
            "--no-progress" => flags.no_progress = true,
            "--quiet" => flags.quiet = true,
            "--verbose" => flags.verbose = flags.verbose.saturating_add(1),
            other => {
                // The JSON flag named with a value (`--json=1`): the value is what is
                // wrong, and the error answers in JSON.
                if other.starts_with("--json=") {
                    flags.json = true;
                } else if other.starts_with("--robot=") {
                    flags.robot = true;
                } else if let Some(value) = other.strip_prefix("--format=") {
                    flags.format = Some(value.to_owned());
                } else if let Some(value) = other.strip_prefix("--progress=") {
                    flags.progress = Some(value.to_owned());
                } else if let Some(vs) = other
                    .strip_prefix('-')
                    .filter(|s| !s.is_empty() && s.bytes().all(|b| b == b'v'))
                {
                    let count = u8::try_from(vs.len()).unwrap_or(u8::MAX);
                    flags.verbose = flags.verbose.saturating_add(count);
                }
            }
        }
    }
    flags
}

/// Whether a raw argv that did not parse asks for an offline run: `--offline` before
/// `--`, or `OMADDONS_OFFLINE` set to exactly `1`.
pub fn offline_from_argv(argv: &[OsString], env: &dyn Fn(&str) -> Option<String>) -> bool {
    env("OMADDONS_OFFLINE").as_deref() == Some("1")
        || argv
            .iter()
            .skip(1)
            .map(|a| a.to_string_lossy())
            .take_while(|a| a != "--")
            .any(|a| a == "--offline")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tty() -> TermEnv {
        TermEnv {
            stdout_tty: true,
            stderr_tty: true,
            ..TermEnv::default()
        }
    }

    fn flags() -> OutputFlags {
        OutputFlags::default()
    }

    #[test]
    fn output_mode_precedence() {
        let env_json = TermEnv {
            format: Some("json".into()),
            ..tty()
        };
        let cases = [
            (flags(), tty(), OutputMode::Human),
            (
                OutputFlags {
                    json: true,
                    ..flags()
                },
                tty(),
                OutputMode::Json,
            ),
            (
                OutputFlags {
                    robot: true,
                    ..flags()
                },
                tty(),
                OutputMode::Json,
            ),
            (
                OutputFlags {
                    format: Some("json".into()),
                    ..flags()
                },
                tty(),
                OutputMode::Json,
            ),
            (flags(), env_json.clone(), OutputMode::Json),
            (
                OutputFlags {
                    format: Some("human".into()),
                    ..flags()
                },
                env_json.clone(),
                OutputMode::Human,
            ),
            (
                OutputFlags {
                    json: true,
                    format: Some("human".into()),
                    ..flags()
                },
                tty(),
                OutputMode::Human,
            ),
        ];
        for (f, env, expected) in cases {
            assert_eq!(resolve(&f, &env).output, expected, "{f:?} {env:?}");
        }
    }

    #[test]
    fn stderr_is_ndjson_without_a_terminal_in_ci_or_on_dumb_terminals() {
        let cases = [
            (tty(), StderrMode::Human),
            (
                TermEnv {
                    stderr_tty: false,
                    ..tty()
                },
                StderrMode::Ndjson,
            ),
            (
                TermEnv {
                    ci: Some("true".into()),
                    ..tty()
                },
                StderrMode::Ndjson,
            ),
            (
                TermEnv {
                    ci: Some(String::new()),
                    ..tty()
                },
                StderrMode::Human,
            ),
            (
                TermEnv {
                    term: Some("dumb".into()),
                    ..tty()
                },
                StderrMode::Ndjson,
            ),
        ];
        for (env, expected) in cases {
            assert_eq!(resolve(&flags(), &env).stderr, expected, "{env:?}");
        }
    }

    #[test]
    fn progress_resolution() {
        let json = OutputFlags {
            json: true,
            ..flags()
        };
        let cases = [
            (flags(), tty(), ProgressMode::Human),
            (json.clone(), tty(), ProgressMode::None),
            (
                OutputFlags {
                    progress: Some("json".into()),
                    ..json
                },
                tty(),
                ProgressMode::Json,
            ),
            (
                OutputFlags {
                    no_progress: true,
                    ..flags()
                },
                tty(),
                ProgressMode::None,
            ),
            (
                flags(),
                TermEnv {
                    ci: Some("1".into()),
                    ..tty()
                },
                ProgressMode::None,
            ),
            (
                flags(),
                TermEnv {
                    stderr_tty: false,
                    ..tty()
                },
                ProgressMode::None,
            ),
            // `TERM=dumb`: stderr is NDJSON, so no human progress line among its events.
            (
                flags(),
                TermEnv {
                    term: Some("dumb".into()),
                    ..tty()
                },
                ProgressMode::None,
            ),
            (
                OutputFlags {
                    progress: Some("none".into()),
                    ..flags()
                },
                tty(),
                ProgressMode::None,
            ),
        ];
        for (f, env, expected) in cases {
            assert_eq!(resolve(&f, &env).progress, expected, "{f:?} {env:?}");
        }
    }

    #[test]
    fn each_condition_disables_color() {
        assert!(resolve(&flags(), &tty()).style.color);
        let off = [
            (
                flags(),
                TermEnv {
                    no_color: Some("1".into()),
                    ..tty()
                },
            ),
            (
                OutputFlags {
                    no_color: true,
                    ..flags()
                },
                tty(),
            ),
            (
                OutputFlags {
                    json: true,
                    ..flags()
                },
                tty(),
            ),
            (
                flags(),
                TermEnv {
                    term: Some("dumb".into()),
                    ..tty()
                },
            ),
            (
                flags(),
                TermEnv {
                    ci: Some("true".into()),
                    ..tty()
                },
            ),
            (
                flags(),
                TermEnv {
                    stderr_tty: false,
                    ..tty()
                },
            ),
            (
                flags(),
                TermEnv {
                    stdout_tty: false,
                    ..tty()
                },
            ),
        ];
        for (f, env) in off {
            let cfg = resolve(&f, &env);
            assert!(!cfg.style.color, "{f:?} {env:?}");
        }
        assert!(
            resolve(
                &flags(),
                &TermEnv {
                    no_color: Some(String::new()),
                    ..tty()
                }
            )
            .style
            .color
        );
    }

    #[test]
    fn raw_argv_finds_the_output_mode() {
        let argv = |args: &[&str]| -> Vec<OsString> {
            std::iter::once("omaddons")
                .chain(args.iter().copied())
                .map(OsString::from)
                .collect()
        };
        let env = tty();
        assert_eq!(
            resolve_output_mode_from_argv(&argv(&["instal", "--json"]), &env),
            OutputMode::Json
        );
        assert_eq!(
            resolve_output_mode_from_argv(&argv(&["--format=json", "x"]), &env),
            OutputMode::Json
        );
        assert_eq!(
            resolve_output_mode_from_argv(&argv(&["--format", "json"]), &env),
            OutputMode::Json
        );
        assert_eq!(
            resolve_output_mode_from_argv(&argv(&["--robot"]), &env),
            OutputMode::Json
        );
        assert_eq!(
            resolve_output_mode_from_argv(&argv(&["x", "--", "--json"]), &env),
            OutputMode::Human
        );
        // The JSON flag with a value clap refuses, and a value-taking flag before it.
        for args in [
            &["status", "--json=1"][..],
            &["status", "--json="],
            &["--robot=yes", "status"],
            &["status", "--format", "--json"],
            &["status", "--progress", "--json"],
        ] {
            assert_eq!(
                resolve_output_mode_from_argv(&argv(args), &env),
                OutputMode::Json,
                "{args:?}"
            );
        }
        let env_json = TermEnv {
            format: Some("JSON".into()),
            ..tty()
        };
        assert_eq!(
            resolve_output_mode_from_argv(&argv(&[]), &env_json),
            OutputMode::Json
        );
    }

    #[test]
    fn raw_argv_finds_every_presentation_flag_and_offline() {
        let argv = |args: &[&str]| -> Vec<OsString> {
            std::iter::once("omaddons")
                .chain(args.iter().copied())
                .map(OsString::from)
                .collect()
        };
        let flags = flags_from_argv(&argv(&[
            "nosuch",
            "--no-color",
            "--quiet",
            "-vv",
            "--verbose",
            "--progress=json",
            "--no-progress",
        ]));
        assert!(flags.no_color && flags.quiet && flags.no_progress);
        assert_eq!(
            (flags.verbose, flags.progress.as_deref()),
            (3, Some("json"))
        );
        assert!(!resolve(&flags, &tty()).style.color);
        let after = flags_from_argv(&argv(&["x", "--", "--no-color", "-v"]));
        assert!(!after.no_color && after.verbose == 0);

        let none = |_: &str| None;
        let one = |name: &str| (name == "OMADDONS_OFFLINE").then(|| "1".to_owned());
        let yes = |name: &str| (name == "OMADDONS_OFFLINE").then(|| "yes".to_owned());
        assert!(offline_from_argv(&argv(&["nosuch", "--offline"]), &none));
        assert!(!offline_from_argv(&argv(&["x", "--", "--offline"]), &none));
        assert!(offline_from_argv(&argv(&["nosuch"]), &one));
        assert!(!offline_from_argv(&argv(&["nosuch"]), &yes), "exactly 1");
    }
}
