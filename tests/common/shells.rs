//! Shell checks for generated completion scripts (A7): find a shell on PATH, check a
//! script's syntax without running it, and drive bash's completion function, zsh's
//! `_omaddons` and fish's `complete -C` (omad-pede) over a case list. Every child
//! gets HOME, ZDOTDIR and XDG_CONFIG_HOME inside the test's own directory, so no rc file
//! of the developer is read, and the real home is never touched. The binary under test
//! comes first on PATH, so a script's `omaddons __complete` runs it, never an installed
//! omaddons; given a [`TestEnv`], it reads that home's files.

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

use serde::Serialize;

use super::env::TestEnv;

/// A shell that can check a script without running it.
#[derive(Debug, Clone, Copy)]
pub enum Checker {
    Bash,
    Zsh,
    Fish,
}

impl Checker {
    pub fn program(self) -> &'static str {
        match self {
            Self::Bash => "bash",
            Self::Zsh => "zsh",
            Self::Fish => "fish",
        }
    }

    /// The arguments before the script's path: parse it, run nothing, read no rc file.
    pub fn args(self) -> &'static [&'static str] {
        match self {
            Self::Bash => &["--norc", "--noprofile", "-n"],
            Self::Zsh => &["-f", "-n"],
            Self::Fish => &["--no-execute"],
        }
    }
}

/// What a shell child did, for the test log.
#[derive(Debug, Clone, Serialize)]
pub struct ShellRun {
    pub argv: Vec<String>,
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub elapsed_ms: u64,
}

/// The shell's absolute path on PATH (every directory of PATH, executable bit set), or
/// None.
pub fn find(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(program))
        .find(|candidate| {
            fs::metadata(candidate)
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
}

/// Writes `script` to `dir/completions.<program>` and runs the checker on it. Ok when the
/// shell exits 0, Err (with the run) otherwise, a missing shell included.
pub fn syntax_check(checker: Checker, script: &str, dir: &Path) -> Result<ShellRun, ShellRun> {
    let file = dir.join(format!("completions.{}", checker.program()));
    fs::write(&file, script).unwrap();
    let Some(shell) = find(checker.program()) else {
        return Err(ShellRun {
            argv: vec![checker.program().to_owned()],
            code: None,
            stdout: String::new(),
            stderr: format!("{} is not on PATH", checker.program()),
            elapsed_ms: 0,
        });
    };
    let mut args: Vec<OsString> = checker.args().iter().map(OsString::from).collect();
    args.push(file.into_os_string());
    let run = run(&shell, &args, dir, &dir.join("shell-home"), None);
    if run.code == Some(0) {
        Ok(run)
    } else {
        Err(run)
    }
}

/// The driver `bash_complete` sources the script with: each line of the cases file is one
/// command line's words separated by `\x1f`, ending in `\x1fEND` so that a trailing empty
/// word survives `read -a` (the `unset` drops `END`). It calls the function the script
/// registered for `omaddons` (`complete -p`), as bash would, and exits 91 when there is
/// none. One output line per case, offers separated by `\x1f`.
const BASH_DRIVER: &str = r#"source "$1" || exit 90
read -r -a spec <<< "$(complete -p omaddons 2>/dev/null)"
fn=
for (( i = 0; i + 1 < ${#spec[@]}; i++ )); do
    [[ ${spec[i]} == -F ]] && fn=${spec[i+1]}
done
[[ -n $fn ]] || exit 91
while IFS=$'\x1f' read -r -a words; do
    unset 'words[${#words[@]}-1]'
    COMP_WORDS=("${words[@]}")
    COMP_CWORD=$(( ${#COMP_WORDS[@]} - 1 ))
    COMP_LINE="${COMP_WORDS[*]}"
    COMP_POINT=${#COMP_LINE}
    COMPREPLY=()
    "$fn" omaddons "${COMP_WORDS[COMP_CWORD]}" "${COMP_WORDS[COMP_CWORD-1]}"
    printf '%s\x1f' "${COMPREPLY[@]}"
    printf '\n'
done < "$2"
"#;

/// What the function bash runs for `omaddons` offers for each case (its words after
/// `omaddons`, the last being the word being completed, possibly empty): one `bash
/// --norc --noprofile` process sources `script` and runs every case, in the empty
/// directory `cwd`, with `env`'s home when given (else an empty one). Panics if bash is
/// missing (never skipped), if the script does not source or registers no function, or
/// if a case gives no line.
pub fn bash_complete(
    script: &Path,
    cases: &[Vec<String>],
    cwd: &Path,
    env: Option<&TestEnv>,
) -> (ShellRun, Vec<Vec<String>>) {
    let bash = find("bash").unwrap_or_else(|| {
        panic!("bash is required by this test and is never skipped (M4 checkpoint criterion 15)") // ubs:ignore — fails the test
    });
    let beside = script.parent().unwrap();
    let driver = beside.join("complete-driver.bash");
    fs::write(&driver, BASH_DRIVER).unwrap();
    let cases_file = beside.join("complete-cases.txt");
    let mut lines = String::new();
    for case in cases {
        assert!(
            case.iter().all(|w| !w.contains(['\x1f', '\n'])),
            "{case:?}: a word holds the separator"
        );
        let words: Vec<&str> = std::iter::once("omaddons")
            .chain(case.iter().map(String::as_str))
            .chain(std::iter::once("END"))
            .collect();
        lines.push_str(&words.join("\x1f"));
        lines.push('\n');
    }
    fs::write(&cases_file, lines).unwrap();
    let args: Vec<OsString> = vec![
        "--norc".into(),
        "--noprofile".into(),
        driver.into_os_string(),
        script.as_os_str().to_owned(),
        cases_file.into_os_string(),
    ];
    // The shell's home beside the script, never in `cwd`: file-name fallbacks list `cwd`.
    let run = run(&bash, &args, cwd, &beside.join("shell-home"), env);
    assert_ne!(
        run.code,
        Some(90),
        "the completion script did not source: {}",
        run.stderr
    );
    assert_ne!(
        run.code,
        Some(91),
        "the completion script registered no function for omaddons: {}",
        run.stderr
    );
    let offers: Vec<Vec<String>> = run
        .stdout
        .lines()
        .map(|line| {
            line.split('\x1f')
                .filter(|offer| !offer.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .collect();
    assert_eq!(
        offers.len(),
        cases.len(),
        "one line per case: {} cases, {} lines; exit {:?}, stderr: {}",
        cases.len(),
        offers.len(),
        run.code,
        run.stderr
    );
    (run, offers)
}

/// The driver `zsh_complete` runs under `zsh -f` (omad-pede). `compadd`, `compset` and
/// `compdef` work only inside a completion widget, so they are shadowed by functions
/// before and after the script is sourced, and so is the static fallback the glue calls,
/// `_omaddons_static`. Each line of the cases file is one command line's words separated
/// by `\x1f`, ending in `\x1fEND` (as for bash). For each case it sets `words`, `CURRENT`
/// and `PREFIX` as zsh would, calls `_omaddons`, and prints one line: the values handed to
/// `compadd -a`, separated by `\x1f`, then `\x1e`, the `IPREFIX` a `compset -P` left, then
/// `\x1e` and `static` when the glue fell back to the static part. Exits 90 when the
/// script does not source and 91 when it defines no `_omaddons`. The driver never names
/// `path`, which zsh ties to `PATH`.
const ZSH_DRIVER: &str = r#"compdef() { : }
source "$1" || exit 90
(( $+functions[_omaddons] )) || exit 91
typeset -g __offers __static
compadd() {
    [[ $1 == -a ]] || return 1
    __offers=${(pj:\x1f:)${(P@)2}}
}
compset() {
    [[ $1 == -P ]] || return 1
    local pattern=$2
    [[ $PREFIX == ${~pattern}* ]] || return 1
    local kept=${(M)PREFIX##${~pattern}}
    IPREFIX=$IPREFIX$kept
    PREFIX=${PREFIX#$kept}
}
_omaddons_static() { __static=static }
while IFS=$'\x1f' read -r -A words; do
    words=("${(@)words[1,-2]}")
    CURRENT=${#words}
    PREFIX=${words[CURRENT]}
    IPREFIX=
    __offers= __static=
    _omaddons
    print -r -- "$__offers"$'\x1e'"$IPREFIX"$'\x1e'"$__static"
done < "$2"
"#;

/// What zsh's `_omaddons` did for one case (omad-pede).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ZshOffer {
    /// The values it handed `compadd -a`, as given: zsh filters them by the word itself.
    pub offered: Vec<String>,
    /// What a `compset -P` moved into `IPREFIX` (`--target=` for `--target=…`).
    pub iprefix: String,
    /// It fell back to clap's static part (`_omaddons_static`).
    pub fell_back: bool,
}

/// What the glue's `_omaddons` offers for each case (its words after `omaddons`, the last
/// being the word being completed, possibly empty): one `zsh -f` process at `zsh` sources
/// `script` and runs every case, in the empty directory `cwd`, with `env`'s home when given
/// (else an empty one). Panics if the script does not source, defines no `_omaddons`, or a
/// case gives no line.
pub fn zsh_complete(
    zsh: &Path,
    script: &Path,
    cases: &[Vec<String>],
    cwd: &Path,
    env: Option<&TestEnv>,
) -> (ShellRun, Vec<ZshOffer>) {
    let beside = script.parent().unwrap();
    let driver = beside.join("complete-driver.zsh");
    fs::write(&driver, ZSH_DRIVER).unwrap();
    let cases_file = beside.join("complete-cases.zsh.txt");
    fs::write(&cases_file, case_lines(cases)).unwrap();
    let args: Vec<OsString> = vec![
        "-f".into(),
        driver.into_os_string(),
        script.as_os_str().to_owned(),
        cases_file.into_os_string(),
    ];
    let run = run(zsh, &args, cwd, &beside.join("shell-home"), env);
    sourced(&run, "_omaddons");
    let offers: Vec<ZshOffer> = run
        .stdout
        .lines()
        .map(|line| {
            let mut fields = line.split('\x1e');
            let offered = fields
                .next()
                .unwrap_or_default()
                .split('\x1f')
                .filter(|v| !v.is_empty())
                .map(str::to_owned)
                .collect();
            let iprefix = fields.next().unwrap_or_default().to_owned();
            let fell_back = fields.next() == Some("static");
            ZshOffer {
                offered,
                iprefix,
                fell_back,
            }
        })
        .collect();
    one_line_per_case(&run, cases.len(), offers.len());
    (run, offers)
}

/// The driver `fish_complete` runs under `fish --no-config` (omad-pede): each line of the
/// cases file is a command line after `omaddons `, and `complete -C` completes it as fish
/// would at its end. One output line per case: the candidates, descriptions stripped,
/// separated by `\x1f`. Exits 90 when the script does not source and 91 when it defines
/// no `__fish_omaddons_values`. Patterns are regexes: fish 4 no longer globs `?`.
const FISH_DRIVER: &str = r#"source $argv[1]; or exit 90
functions -q __fish_omaddons_values; or exit 91
while read -l line
    set -l offers (complete -C"omaddons $line" | string replace -r '\t.*$' '')
    # `string join` fails on a single string, so the list itself says whether it is empty.
    if set -q offers[1]
        string join \x1f -- $offers
    else
        echo
    end
end < $argv[2]
"#;

/// What fish completes for each case (its words after `omaddons`, the last being the word
/// being completed, possibly empty, joined by spaces), in the empty directory `cwd`, with
/// `env`'s home when given: one `fish --no-config` process at `fish` sources `script` and
/// runs `complete -C` for every case. fish filters the candidates by the word itself.
/// Panics if a word holds whitespace, the script does not source or defines no
/// `__fish_omaddons_values`, or a case gives no line.
pub fn fish_complete(
    fish: &Path,
    script: &Path,
    cases: &[Vec<String>],
    cwd: &Path,
    env: Option<&TestEnv>,
) -> (ShellRun, Vec<Vec<String>>) {
    let beside = script.parent().unwrap();
    let driver = beside.join("complete-driver.fish");
    fs::write(&driver, FISH_DRIVER).unwrap();
    let cases_file = beside.join("complete-cases.fish.txt");
    let mut lines = String::new();
    for case in cases {
        assert!(
            case.iter().all(|w| !w.contains(char::is_whitespace)),
            "{case:?}: a word holds whitespace"
        );
        lines.push_str(&case.join(" "));
        lines.push('\n');
    }
    fs::write(&cases_file, lines).unwrap();
    let args: Vec<OsString> = vec![
        "--no-config".into(),
        driver.into_os_string(),
        script.as_os_str().to_owned(),
        cases_file.into_os_string(),
    ];
    let run = run(fish, &args, cwd, &beside.join("shell-home"), env);
    sourced(&run, "__fish_omaddons_values");
    let offers: Vec<Vec<String>> = run
        .stdout
        .lines()
        .map(|line| {
            line.split('\x1f')
                .filter(|v| !v.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .collect();
    one_line_per_case(&run, cases.len(), offers.len());
    (run, offers)
}

/// The cases file the bash and zsh drivers read: each case's words after `omaddons`,
/// separated by `\x1f`, ending in `END`.
fn case_lines(cases: &[Vec<String>]) -> String {
    let mut lines = String::new();
    for case in cases {
        assert!(
            case.iter().all(|w| !w.contains(['\x1f', '\x1e', '\n'])),
            "{case:?}: a word holds a separator"
        );
        let words: Vec<&str> = std::iter::once("omaddons")
            .chain(case.iter().map(String::as_str))
            .chain(std::iter::once("END"))
            .collect();
        lines.push_str(&words.join("\x1f"));
        lines.push('\n');
    }
    lines
}

/// The script sourced (driver exit 90 otherwise) and defined `function` (91).
fn sourced(run: &ShellRun, function: &str) {
    assert_ne!(
        run.code,
        Some(90),
        "the completion script did not source: {}",
        run.stderr
    );
    assert_ne!(
        run.code,
        Some(91),
        "the completion script defined no {function}: {}",
        run.stderr
    );
}

fn one_line_per_case(run: &ShellRun, cases: usize, lines: usize) {
    assert_eq!(
        lines, cases,
        "one line per case: {cases} cases, {lines} lines; exit {:?}, stdout: {:?}, stderr: {}",
        run.code, run.stdout, run.stderr
    );
}

/// PATH for a shell child: the directory of the binary under test, then this process's.
fn path_with_binary() -> OsString {
    let binary = Path::new(env!("CARGO_BIN_EXE_omaddons"));
    let path = std::env::var_os("PATH").unwrap_or_default();
    let dirs = binary
        .parent()
        .map(Path::to_path_buf)
        .into_iter()
        .chain(std::env::split_paths(&path));
    std::env::join_paths(dirs).unwrap()
}

/// Runs `program` with `args` in `cwd`, with PATH (the binary under test first), and
/// HOME, ZDOTDIR and XDG_CONFIG_HOME under `home` (inside the test's directory); then
/// `env`'s variables, when given, so omaddons reads that home.
fn run(
    program: &Path,
    args: &[OsString],
    cwd: &Path,
    home: &Path,
    env: Option<&TestEnv>,
) -> ShellRun {
    fs::create_dir_all(home).unwrap();
    let mut command = Command::new(program); // ubs:ignore — bash, zsh or fish, found on PATH by `find`
    command
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .env("PATH", path_with_binary())
        .env("HOME", home)
        .env("ZDOTDIR", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("LC_ALL", "C")
        .stdin(Stdio::null());
    if let Some(env) = env {
        env.apply_to(&mut command);
    }
    let started = Instant::now();
    let output = command.output().unwrap();
    ShellRun {
        argv: std::iter::once(program.display().to_string())
            .chain(args.iter().map(|a| a.to_string_lossy().into_owned()))
            .collect(),
        code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
    }
}
