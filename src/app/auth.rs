//! `omaddons auth github [status | set | clear | --from-gh]`: the opt-in GitHub token
//! (vision D12). The main GitHub path needs none; a token raises GitHub's REST limit
//! from 60 to 5,000 requests an hour (search: 10 to 30 a minute) for the prerelease
//! channels, repositories without `release.json`, and `search`.
//!
//! `set` reads the token from standard input (one line; never argv, and never a terminal:
//! it refuses rather than wait) and writes `<config>/omaddons/credentials/github`, mode
//! 0600 in a 0700 directory. `--from-gh` stores what `gh auth token` prints: the only time
//! omaddons asks `gh`, and only on request. `clear` empties the file (an empty file is no
//! token). `status` says which token is used and, online, how much of the hour's REST
//! budget is left (one request to `/rate_limit`, which costs none). The token itself is
//! never shown, and no endpoint that names the account is called.

use std::io::{BufRead, Read, Write};
use std::path::PathBuf;
use std::sync::Arc;

use jiff::Timestamp;
use schemars::JsonSchema;
use serde::Serialize;

use crate::cli::Context;
use crate::error::{Error, ErrorCode, Result, Suggestion, Warning};
use crate::fsx::{
    Content, GlobalLock, Op, ScopeTable, WriteSession, Writer, ensure_dir_all, mutate,
};
use crate::limits::AUTH_TOKEN_MAX_BYTES;
use crate::net::{NetConfig, NetLog, Request, stack};
use crate::output::{Outcome, Style};
use crate::sources::github_rest::TOKEN_VAR;
use crate::sources::github_token::{GithubToken, TokenSource, valid};
use crate::state::config::{Config, CredentialRef, resolve_credential};
use crate::state::paths::Paths;

/// `auth`'s arguments: the service, and `--from-gh` before or after it.
#[derive(Debug, Clone, clap::Args)]
pub struct AuthArgs {
    /// Store the token `gh auth token` prints (the only time omaddons asks gh)
    #[arg(long, global = true)]
    pub from_gh: bool,
    #[command(subcommand)]
    pub service: AuthService,
}

/// The services a token can be stored for.
#[derive(Debug, Clone, clap::Subcommand)]
pub enum AuthService {
    /// The GitHub token: status (the default), set (from standard input) or clear
    #[command(after_long_help = AUTH_HELP)]
    Github(GithubArgs),
}

impl AuthService {
    /// The envelope `command`.
    pub const fn command(&self) -> &'static str {
        match self {
            Self::Github(_) => "auth github",
        }
    }
}

/// `auth github`'s own arguments.
#[derive(Debug, Clone, clap::Args)]
pub struct GithubArgs {
    /// status (the default), set (the token from standard input) or clear
    #[arg(value_enum, value_name = "ACTION")]
    pub action: Option<AuthAction>,
}

/// `auth github`'s long help.
const AUTH_HELP: &str = "\
Examples:
  omaddons auth github                        which token is used, and the hour's REST budget
  echo \"$TOKEN\" | omaddons auth github set    store a token (standard input only, never argv)
  omaddons auth github --from-gh              store the token gh auth token prints
  omaddons auth github clear                  forget the stored token

The main GitHub path needs no token. A token goes only to api.github.com; GITHUB_TOKEN
and gh's credentials are never read unless you ask with --from-gh. With --dry-run, set
and clear check the token and write nothing.";

/// What `auth github` does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum AuthAction {
    Status,
    Set,
    Clear,
}

/// `auth github`'s data: the token in use after the command (under `--dry-run`, as it
/// would be), never the token itself.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct AuthData {
    pub configured: bool,
    pub source: TokenSource,
    /// `status` online: the core REST budget; `null` offline and after `set` or `clear`.
    pub rate_limit: Option<RateLimit>,
    /// `--dry-run`: nothing was written.
    pub dry_run: bool,
}

/// GitHub's core REST budget.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct RateLimit {
    pub limit: u64,
    pub remaining: u64,
    #[schemars(with = "String")]
    pub reset_at: Timestamp,
}

/// `auth github`'s outcome.
#[derive(Debug)]
pub struct AuthOutcome {
    data: AuthData,
    warnings: Vec<Warning>,
    action: AuthAction,
    /// The credentials file.
    path: PathBuf,
}

impl Outcome for AuthOutcome {
    type Data = AuthData;

    fn command(&self) -> &'static str {
        "auth github"
    }

    fn data(&self) -> &AuthData {
        &self.data
    }

    fn warnings(&self) -> &[Warning] {
        &self.warnings
    }

    fn render_human(&self, w: &mut dyn Write, _style: &Style) -> std::io::Result<()> {
        let d = &self.data;
        let path = self.path.display();
        match (self.action, d.dry_run) {
            (AuthAction::Set, false) => writeln!(w, "stored the GitHub token in {path}")?,
            (AuthAction::Set, true) => writeln!(w, "would store the GitHub token in {path}")?,
            (AuthAction::Clear, false) => writeln!(w, "cleared {path}")?,
            (AuthAction::Clear, true) => writeln!(w, "would clear {path}")?,
            (AuthAction::Status, _) => {}
        }
        let whence = match d.source {
            TokenSource::Env => format!("from {TOKEN_VAR}"),
            TokenSource::Config => "from config.toml's [sources.github] credential".to_owned(),
            TokenSource::File => format!("from {path}"),
            TokenSource::None => String::new(),
        };
        if d.configured {
            writeln!(w, "GitHub token: {whence}")?;
        } else {
            writeln!(
                w,
                "no GitHub token: GitHub's API allows 60 requests an hour (the main path needs none)"
            )?;
        }
        if matches!(self.action, AuthAction::Set) && matches!(d.source, TokenSource::Env) {
            writeln!(w, "{TOKEN_VAR} is set, and is used instead of the file")?;
        }
        if let Some(r) = &d.rate_limit {
            writeln!(
                w,
                "{} of {} requests left until {}",
                r.remaining,
                r.limit,
                r.reset_at.strftime("%Y-%m-%d %H:%M UTC")
            )?;
        }
        Ok(())
    }
}

/// Runs `auth github`.
pub fn run(ctx: &Context, args: &AuthArgs) -> Result<AuthOutcome> {
    let AuthService::Github(github) = &args.service;
    let action = match (github.action, args.from_gh) {
        (Some(AuthAction::Status | AuthAction::Clear), true) => {
            return Err(Error::new(
                ErrorCode::Usage,
                "--from-gh stores gh's token: it goes with set, not status or clear",
                Suggestion::command(["omaddons", "auth", "github", "set", "--from-gh"]),
            ));
        }
        (Some(action), _) => action,
        (None, true) => AuthAction::Set,
        (None, false) => AuthAction::Status,
    };
    let paths = Paths::from_env()?;
    let path = paths.credential_file("github");
    match action {
        AuthAction::Set => {
            let token = if args.from_gh {
                from_gh()?
            } else {
                from_stdin(ctx)?
            };
            write(ctx, &paths, token.as_bytes())?;
        }
        AuthAction::Clear => write(ctx, &paths, b"")?,
        AuthAction::Status => {}
    }
    let config = Config::load(&paths)?;
    let mut warnings = config.warnings;
    let token = GithubToken::resolve(&paths, &config.config.sources);
    warnings.extend(token.warnings.iter().cloned());
    ctx.notes.warn_all(&warnings);
    // Offline, a credential command never runs: `configured` then says one is
    // configured, not that it works. One that is had without running anything (env,
    // file) is still checked.
    let rate_limit = match action {
        AuthAction::Status if ctx.offline => {
            if let Some(resolved) = token.peek() {
                resolved?;
            }
            None
        }
        AuthAction::Status => {
            // A configured token that cannot be had is the answer.
            token.value()?;
            Some(rate_limit(ctx, &paths, &config.config, &token)?)
        }
        AuthAction::Set | AuthAction::Clear => None,
    };
    // A dry run describes the token as the write would leave it: the environment and a
    // config.toml credential outrank the file, so only a file token (or none) changes.
    let (configured, source) = match (action, ctx.dry_run, token.source()) {
        (AuthAction::Set, true, TokenSource::File | TokenSource::None) => (true, TokenSource::File),
        (AuthAction::Clear, true, TokenSource::File | TokenSource::None) => {
            (false, TokenSource::None)
        }
        _ => (token.configured(), token.source()),
    };
    Ok(AuthOutcome {
        data: AuthData {
            configured,
            source,
            rate_limit,
            dry_run: ctx.dry_run,
        },
        warnings,
        action,
        path,
    })
}

/// The token's line from standard input; a terminal is refused at once.
fn from_stdin(ctx: &Context) -> Result<String> {
    read_token(ctx.stdin_is_tty, &mut std::io::stdin().lock())
}

/// The token's line from `input`, refused before any read when it is a terminal.
fn read_token(is_tty: bool, input: &mut dyn BufRead) -> Result<String> {
    let fix = || Suggestion::text("echo \"$TOKEN\" | omaddons auth github set");
    if is_tty {
        return Err(Error::new(
            ErrorCode::Usage,
            "auth github set reads the token from standard input, not a terminal",
            fix(),
        ));
    }
    let mut line = String::new();
    input
        .take(AUTH_TOKEN_MAX_BYTES + 1)
        .read_line(&mut line)
        .map_err(|e| {
            Error::new(
                ErrorCode::Usage,
                format!("could not read the token from standard input: {e}"),
                fix(),
            )
        })?;
    if line.len() as u64 > AUTH_TOKEN_MAX_BYTES {
        return Err(Error::new(
            ErrorCode::Usage,
            format!("the token's line is longer than {AUTH_TOKEN_MAX_BYTES} bytes"),
            fix(),
        ));
    }
    checked(line.trim(), fix())
}

/// What `gh auth token` prints, asked once, on request.
fn from_gh() -> Result<String> {
    let on_path = std::env::var_os("PATH")
        .is_some_and(|p| std::env::split_paths(&p).any(|dir| dir.join("gh").is_file()));
    if !on_path {
        return Err(Error::new(
            ErrorCode::AuthRequired,
            "gh is not installed (not on PATH)",
            Suggestion::text("echo \"$TOKEN\" | omaddons auth github set"),
        ));
    }
    let command = CredentialRef::Command {
        command: vec!["gh".into(), "auth".into(), "token".into()],
    };
    let (secret, _) = resolve_credential(&command).map_err(|e| {
        Error::new(
            ErrorCode::AuthRequired,
            format!("gh auth token gave no token: {}", e.message),
            Suggestion::text("run gh auth login, or pipe a token to omaddons auth github set"),
        )
    })?;
    checked(
        secret.expose().trim(),
        Suggestion::text("run gh auth login, or pipe a token to omaddons auth github set"),
    )
}

/// `token` when a header can carry it.
fn checked(token: &str, fix: Suggestion) -> Result<String> {
    if valid(token) {
        Ok(token.to_owned())
    } else if token.is_empty() {
        Err(Error::new(ErrorCode::Usage, "no token was given", fix))
    } else {
        Err(Error::new(
            ErrorCode::Usage,
            "the token holds spaces or control characters, which a token cannot have",
            fix,
        ))
    }
}

/// Writes the credentials file (mode 0600, in a 0700 directory) under the global lock.
fn write(ctx: &Context, paths: &Paths, content: &[u8]) -> Result<()> {
    if ctx.dry_run {
        return Ok(());
    }
    // A `locked` refusal's fix reruns this command (the command boundary sets it).
    let guard = GlobalLock::acquire(paths, "auth github")?;
    {
        use crate::engine::journal::{no_saved_variables, recover_under};
        ctx.notes.warn_all(&recover_under(
            &guard,
            paths,
            &no_saved_variables,
            ctx.command,
            ctx.actor,
        ));
    }
    let scopes = ScopeTable::new(paths, &[], None)?;
    let mut s = WriteSession::untracked(Writer::Auth, &scopes, "credentials");
    let dir = paths.credentials_dir();
    let file = paths.credential_file("github");
    // A change to nothing records none, and writes nothing: a missing file and an empty
    // one are both no token. One that cannot be read is rewritten.
    let stored = match std::fs::read(&file) {
        Ok(bytes) => Some(bytes),
        // ubs:ignore — an io::ErrorKind, not a secret
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(Vec::new()),
        Err(_) => None,
    };
    // ubs:ignore — whether the stored token changes, never shown
    if stored.as_deref() == Some(content) {
        // Still tightens a directory others could read: a repair, not a change.
        if dir.is_dir() {
            mutate(&mut s, &dir, Op::Chmod { mode: 0o700 })?;
        }
        return Ok(());
    }
    ensure_dir_all(&mut s, paths.config_dir())?;
    if !dir.is_dir() {
        mutate(&mut s, &dir, Op::CreateDir)?;
    }
    // Also tightens a directory others could read.
    mutate(&mut s, &dir, Op::Chmod { mode: 0o700 })?;
    mutate(
        &mut s,
        &file,
        Op::WriteFile {
            content: Content::Bytes(content),
            mode: Some(0o600),
        },
    )?;
    // Who stored or cleared it, never the token: it came on standard input.
    super::record_setting(ctx, &mut s, paths, crate::model::TxKind::Auth, None);
    Ok(())
}

/// GitHub's core REST budget: one request to `/rate_limit`, which spends none.
fn rate_limit(
    ctx: &Context,
    paths: &Paths,
    config: &Config,
    token: &Arc<GithubToken>,
) -> Result<RateLimit> {
    let net = NetConfig {
        offline: ctx.offline,
        git_hosts: config.git_hosts.clone(),
        ..NetConfig::default()
    };
    let http = stack(&net, NetLog::new(paths)?, paths);
    let mut req = Request::get("https://api.github.com/rate_limit", "rate-limit")
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28");
    let sent = token.value()?;
    if let Some(t) = sent {
        // ubs:ignore — an outbound request header; github_token::valid rejects controls and spaces
        req = req.header("Authorization", format!("Bearer {t}"));
    }
    let resp = http.send(req)?;
    if resp.status == 401 && sent.is_some() {
        return Err(token.rejected());
    }
    let body = resp.error_for_status("GitHub's rate_limit API")?.bytes()?;
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
    let core = &json["resources"]["core"];
    let reset = core["reset"]
        .as_i64()
        .and_then(|s| Timestamp::from_second(s).ok());
    match (core["limit"].as_u64(), core["remaining"].as_u64(), reset) {
        (Some(limit), Some(remaining), Some(reset_at)) => Ok(RateLimit {
            limit,
            remaining,
            reset_at,
        }),
        _ => Err(Error::new(
            ErrorCode::Network,
            "GitHub's rate_limit API answered something unreadable",
            Suggestion::text("retry later; if it persists, GitHub changed its API"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Input whose every read fails, so a read shows in the error.
    struct Untouchable;

    impl Read for Untouchable {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("a terminal was read"))
        }
    }

    impl BufRead for Untouchable {
        fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
            Err(std::io::Error::other("a terminal was read"))
        }

        fn consume(&mut self, _: usize) {}
    }

    #[test]
    fn a_terminal_is_refused_before_any_read() {
        let err = read_token(true, &mut Untouchable).unwrap_err();
        assert_eq!(err.code, ErrorCode::Usage);
        assert!(err.message.contains("not a terminal"), "{}", err.message);
        assert!(err.fix.display().contains("| omaddons auth github set"));
    }

    #[test]
    fn the_first_line_trimmed_and_nothing_a_header_cannot_carry() {
        let read = |s: &str| read_token(false, &mut s.as_bytes());
        assert_eq!(read("ghp_abc\nignored\n").unwrap(), "ghp_abc");
        assert_eq!(read("  ghp_abc \r\n").unwrap(), "ghp_abc");
        for bad in ["", "\n", "two words\n", "a\u{7}b\n"] {
            assert_eq!(read(bad).unwrap_err().code, ErrorCode::Usage, "{bad:?}");
        }
        let long = "x".repeat(AUTH_TOKEN_MAX_BYTES as usize + 10);
        assert_eq!(read(&long).unwrap_err().code, ErrorCode::Usage);
    }

    /// What a run prints: a dry run says what it would do, and the token as it would be.
    #[test]
    fn a_dry_run_says_would() {
        let render = |action, dry_run, configured, source| {
            let outcome = AuthOutcome {
                data: AuthData {
                    configured,
                    source,
                    rate_limit: None,
                    dry_run,
                },
                warnings: Vec::new(),
                action,
                path: PathBuf::from("/c/omaddons/credentials/github"),
            };
            let mut out = Vec::new();
            outcome.render_human(&mut out, &Style::default()).unwrap();
            String::from_utf8(out).unwrap()
        };
        let file = "/c/omaddons/credentials/github";
        let set = render(AuthAction::Set, true, true, TokenSource::File);
        assert_eq!(
            set,
            format!("would store the GitHub token in {file}\nGitHub token: from {file}\n")
        );
        let stored = render(AuthAction::Set, false, true, TokenSource::File);
        assert!(stored.starts_with(&format!("stored the GitHub token in {file}\n")));
        let clear = render(AuthAction::Clear, true, false, TokenSource::None);
        assert!(
            clear.starts_with(&format!("would clear {file}\nno GitHub token")),
            "{clear}"
        );
        let cleared = render(AuthAction::Clear, false, false, TokenSource::None);
        assert!(cleared.starts_with(&format!("cleared {file}\n")));
        let status = render(AuthAction::Status, true, true, TokenSource::File);
        assert_eq!(status, format!("GitHub token: from {file}\n"));
    }
}
