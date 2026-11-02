//! The GitHub token REST requests carry, if any (vision D12: explicit, never implicit).
//! In order: `OMADDONS_GITHUB_TOKEN`; else config.toml's `[sources.github] credential`
//! (an `env`, `file` or `command` reference); else `<config>/omaddons/credentials/github`,
//! as `omaddons auth github set` writes it (an empty file is no token). `GITHUB_TOKEN`
//! and `gh`'s own credentials are never read; only `auth github --from-gh` asks `gh`,
//! once, on request.
//!
//! The main GitHub path (`releases/latest`, `release.json`, feeds) needs no token: it goes
//! only to `api.github.com` (the REST fallback, prerelease channels and search), and a
//! redirect to another host drops it. A configured credential that cannot be resolved is
//! kept as an error and raised by the first REST request, so a command that never calls
//! REST is unaffected; a `command` credential runs only then.

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

use schemars::JsonSchema;
use serde::Serialize;

use super::github_rest::{TOKEN_VAR, token_from};
use crate::error::{Error, ErrorCode, Result, Suggestion, Warning};
use crate::state::config::{CredentialRef, SourceConfig, resolve_credential};
use crate::state::paths::Paths;

/// Where the token comes from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TokenSource {
    /// `OMADDONS_GITHUB_TOKEN`.
    Env,
    /// config.toml's `[sources.github] credential`.
    Config,
    /// `<config>/omaddons/credentials/github`.
    File,
    #[default]
    None,
}

/// The token for one command, resolved once.
pub struct GithubToken {
    source: TokenSource,
    /// The token, or why a configured one could not be had; resolved on first use for a
    /// `command` credential.
    value: OnceLock<std::result::Result<Option<String>, Error>>,
    /// A `command` credential, run on first use.
    command: Option<Vec<String>>,
    /// Raised while resolving (`credential_file_mode`).
    pub warnings: Vec<Warning>,
}

impl std::fmt::Debug for GithubToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GithubToken")
            .field("source", &self.source)
            .finish_non_exhaustive()
    }
}

impl GithubToken {
    /// No token.
    pub fn none() -> Arc<Self> {
        Self::fixed(TokenSource::None, None)
    }

    /// `token` as if from `source` (tests, and the clients' own constructors).
    pub fn fixed(source: TokenSource, token: Option<String>) -> Arc<Self> {
        let value = OnceLock::new();
        let _ = value.set(Ok(token.filter(|t| valid(t))));
        Arc::new(Self {
            source,
            value,
            command: None,
            warnings: Vec::new(),
        })
    }

    /// The token this machine is configured with, from the process environment.
    pub fn resolve(paths: &Paths, sources: &BTreeMap<String, SourceConfig>) -> Arc<Self> {
        Self::resolve_with(&|name| std::env::var(name).ok(), paths, sources)
    }

    /// [`GithubToken::resolve`] with `env` for the environment.
    /// Whether a token is configured, in [`GithubToken::resolve_with`]'s order, by presence
    /// alone: the variable is set; or a configured reference names a credential that is
    /// there (`config::credential_present`: no file read, no command run); or the stored
    /// file is a non-empty regular file, never opened. "Configured" is not "valid"
    /// (M4-D4: `capabilities` reads nothing secret).
    pub fn present(paths: &Paths, sources: &BTreeMap<String, SourceConfig>) -> bool {
        Self::present_with(&|name| std::env::var(name).ok(), paths, sources)
    }

    /// [`GithubToken::present`] with the environment looked up through `env`.
    pub fn present_with(
        env: &dyn Fn(&str) -> Option<String>,
        paths: &Paths,
        sources: &BTreeMap<String, SourceConfig>,
    ) -> bool {
        if token_from(env).is_some() {
            return true;
        }
        if let Some(credential) = sources.get("github").and_then(|s| s.credential.as_ref()) {
            return crate::state::config::credential_present_with(credential, &|name| {
                env(name).map(std::ffi::OsString::from)
            });
        }
        crate::state::config::non_empty_file(&paths.credential_file("github"))
    }

    pub fn resolve_with(
        env: &dyn Fn(&str) -> Option<String>,
        paths: &Paths,
        sources: &BTreeMap<String, SourceConfig>,
    ) -> Arc<Self> {
        // A malformed variable is an error the first REST request raises, as a malformed
        // configured credential is: never a silent run without the token.
        if let Some(token) = token_from(env) {
            return Self::resolved(
                TokenSource::Env,
                checked(&token, TokenSource::Env),
                Vec::new(),
            );
        }
        let configured = sources.get("github").and_then(|s| s.credential.as_ref());
        if let Some(credential) = configured {
            if let CredentialRef::Command { command } = credential {
                return Arc::new(Self {
                    source: TokenSource::Config,
                    value: OnceLock::new(),
                    command: Some(command.clone()),
                    warnings: Vec::new(),
                });
            }
            let (value, warnings) = match resolve_credential(credential) {
                Ok((secret, warnings)) => (checked(secret.expose(), TokenSource::Config), warnings),
                Err(error) => (Err(error), Vec::new()),
            };
            return Self::resolved(TokenSource::Config, value, warnings);
        }
        let file = paths.credential_file("github");
        let stored = std::fs::read_to_string(&file).unwrap_or_default();
        if stored.trim().is_empty() {
            return Self::none();
        }
        // Through the credential reader, for its file-mode warning.
        let (value, warnings) = match resolve_credential(&CredentialRef::File { file }) {
            Ok((secret, warnings)) => (checked(secret.expose(), TokenSource::File), warnings),
            Err(error) => (Err(error), Vec::new()),
        };
        Self::resolved(TokenSource::File, value, warnings)
    }

    fn resolved(
        source: TokenSource,
        value: std::result::Result<Option<String>, Error>,
        warnings: Vec<Warning>,
    ) -> Arc<Self> {
        let cell = OnceLock::new();
        let _ = cell.set(value);
        Arc::new(Self {
            source,
            value: cell,
            command: None,
            warnings,
        })
    }

    /// Where the token comes from (`none` when there is none to look for).
    pub fn source(&self) -> TokenSource {
        self.source
    }

    /// The token; a configured credential that cannot be resolved is the error (a
    /// `command` credential runs now, once).
    pub fn value(&self) -> Result<Option<&str>> {
        self.value
            .get_or_init(|| match &self.command {
                Some(command) => resolve_credential(&CredentialRef::Command {
                    command: command.clone(),
                })
                .and_then(|(secret, _)| checked(secret.expose(), TokenSource::Config)),
                None => Ok(None),
            })
            .as_ref()
            .map(Option::as_deref)
            .map_err(Clone::clone)
    }

    /// A `command` credential from config, not run yet (tests).
    #[cfg(test)]
    pub(crate) fn command(command: Vec<String>) -> Arc<Self> {
        Arc::new(Self {
            source: TokenSource::Config,
            value: OnceLock::new(),
            command: Some(command),
            warnings: Vec::new(),
        })
    }

    /// The token when having it runs nothing (an `env` or `file` credential, or a
    /// command that already ran); `None` for a `command` credential not run yet.
    pub fn peek(&self) -> Option<Result<Option<&str>>> {
        self.value.get().map(|resolved| {
            resolved
                .as_ref()
                .map(Option::as_deref)
                .map_err(Clone::clone)
        })
    }

    /// Whether a credential is configured, without running a `command` credential: a
    /// token resolved, or a command not run yet (whether it works is known only when a
    /// REST request runs it). `false` for none, and for one that could not be had.
    pub fn configured(&self) -> bool {
        match self.value.get() {
            Some(resolved) => matches!(resolved, Ok(Some(_))),
            None => self.command.is_some(),
        }
    }

    /// `auth_rejected`, for a 401 answered to this token: the fix undoes where it came
    /// from.
    pub fn rejected(&self) -> Error {
        let clear = Suggestion::command(["omaddons", "auth", "github", "clear"]);
        let set = Suggestion::command(["omaddons", "auth", "github", "set"]);
        let message = "GitHub rejected the token (401): it is wrong, expired or revoked";
        match self.source {
            TokenSource::Env => Error::new(
                ErrorCode::AuthRejected,
                format!("{message}; it comes from {TOKEN_VAR}"),
                Suggestion::text(format!("replace or unset {TOKEN_VAR}")),
            )
            .alternative(clear),
            TokenSource::Config => Error::new(
                ErrorCode::AuthRejected,
                format!("{message}; it comes from config.toml's [sources.github] credential"),
                Suggestion::text("correct [sources.github] credential in config.toml"),
            )
            .alternative(clear),
            TokenSource::File | TokenSource::None => {
                Error::new(ErrorCode::AuthRejected, message, clear).alternative(set)
            }
        }
    }
}

/// A token as a header can carry it: not blank, no control characters, no spaces.
pub fn valid(token: &str) -> bool {
    !token.is_empty() && token.chars().all(|c| !c.is_control() && !c.is_whitespace())
}

/// `secret` trimmed, if it can be a token; else `auth_required` naming `source`.
fn checked(secret: &str, source: TokenSource) -> std::result::Result<Option<String>, Error> {
    let token = secret.trim();
    if valid(token) {
        return Ok(Some(token.to_owned()));
    }
    let fix = Suggestion::command(["omaddons", "auth", "github", "set"]);
    let (whence, fix) = match source {
        TokenSource::Config => ("config.toml's [sources.github] credential", fix),
        TokenSource::Env => (
            TOKEN_VAR,
            Suggestion::text(format!("replace or unset {TOKEN_VAR}")),
        ),
        _ => ("the stored GitHub token", fix),
    };
    Err(Error::new(
        ErrorCode::AuthRequired,
        format!("{whence} is empty or holds characters a token cannot have"),
        fix,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(dir: &std::path::Path) -> Paths {
        Paths::with_roots(
            dir.join("home"),
            dir.join("config"),
            dir.join("state"),
            dir.join("cache"),
        )
    }

    fn store(paths: &Paths, text: &str) {
        let file = paths.credential_file("github");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, text).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    fn env(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |name| {
            pairs
                .iter()
                // ubs:ignore — comparing environment variable names, not secrets
                .find(|(k, _)| *k == name)
                .map(|(_, v)| (*v).to_owned())
        }
    }

    #[test]
    fn env_then_config_then_file_and_never_github_token() {
        let dir = tempfile::tempdir().unwrap();
        let p = paths(dir.path());
        let none = BTreeMap::new();
        let resolve = |e: &dyn Fn(&str) -> Option<String>, s: &BTreeMap<String, SourceConfig>| {
            let t = GithubToken::resolve_with(e, &p, s);
            (
                t.source(),
                t.value().map(|v| v.map(str::to_owned)).ok().flatten(),
            )
        };
        assert_eq!(
            resolve(&env(&[("GITHUB_TOKEN", "x")]), &none),
            (TokenSource::None, None)
        );
        store(&p, "from-file\n");
        assert_eq!(
            resolve(&env(&[]), &none),
            (TokenSource::File, Some("from-file".into()))
        );
        let config = BTreeMap::from([(
            "github".to_owned(),
            SourceConfig {
                enabled: None,
                credential: Some(CredentialRef::Command {
                    command: vec!["sh".into(), "-c".into(), "echo from-config".into()],
                }),
            },
        )]);
        assert_eq!(
            resolve(&env(&[]), &config),
            (TokenSource::Config, Some("from-config".into()))
        );
        assert_eq!(
            resolve(&env(&[(TOKEN_VAR, "from-env")]), &config),
            (TokenSource::Env, Some("from-env".into()))
        );
        // An empty file is no token.
        store(&p, "");
        assert_eq!(resolve(&env(&[]), &none), (TokenSource::None, None));
    }

    #[test]
    fn a_broken_configured_credential_is_an_error_not_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = paths(dir.path());
        store(&p, "from-file");
        let config = BTreeMap::from([(
            "github".to_owned(),
            SourceConfig {
                enabled: None,
                credential: Some(CredentialRef::Env {
                    env: "OMADDONS_TEST_NO_SUCH_VAR".into(),
                }),
            },
        )]);
        let t = GithubToken::resolve_with(&env(&[]), &p, &config);
        assert_eq!(t.source(), TokenSource::Config);
        assert_eq!(t.value().unwrap_err().code, ErrorCode::AuthRequired);
        // A token a header cannot carry is refused too.
        store(&p, "two words");
        let t = GithubToken::resolve_with(&env(&[]), &p, &BTreeMap::new());
        assert!(t.value().is_err());
        // So is such a variable: an error, never a silent run without the token.
        let t =
            GithubToken::resolve_with(&env(&[(TOKEN_VAR, "ghp_abc def")]), &p, &BTreeMap::new());
        assert_eq!(t.source(), TokenSource::Env);
        let err = t.value().unwrap_err();
        assert_eq!(err.code, ErrorCode::AuthRequired);
        assert!(err.message.contains(TOKEN_VAR), "{}", err.message);
    }

    #[test]
    fn a_rejected_token_names_where_it_came_from() {
        let file = GithubToken::fixed(TokenSource::File, Some("t".into())).rejected();
        assert_eq!(
            (file.code, file.fix.display()),
            (ErrorCode::AuthRejected, "omaddons auth github clear")
        );
        let env = GithubToken::fixed(TokenSource::Env, Some("t".into())).rejected();
        assert!(env.fix.display().contains(TOKEN_VAR));
        assert_eq!(env.alternatives.len(), 1);
    }

    /// `configured` never runs a command credential: unrun, it counts as configured;
    /// once run, it is what the command gave.
    #[test]
    fn configured_never_runs_a_command() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("ran");
        let script = |out: &str| {
            vec![
                "sh".to_owned(),
                "-c".to_owned(),
                format!("echo x >> '{}'; {out}", marker.display()),
            ]
        };
        let works = GithubToken::command(script("echo ghp_ok"));
        assert!(works.configured());
        assert!(!marker.exists(), "configured() ran the command");
        assert_eq!(works.value().unwrap(), Some("ghp_ok"));
        assert!(works.configured());
        let broken = GithubToken::command(script("exit 3"));
        assert!(broken.configured(), "unrun: configured");
        assert!(broken.value().is_err());
        assert!(!broken.configured(), "run, and it gave nothing");
        assert!(!GithubToken::none().configured());
        assert!(GithubToken::fixed(TokenSource::Env, Some("t".into())).configured());
    }

    #[test]
    fn github_credential_presence_follows_the_resolution_order() {
        use std::os::unix::fs::PermissionsExt;
        let reference = |text: &str| -> BTreeMap<String, SourceConfig> {
            let config: crate::state::config::Config = toml::from_str(text).unwrap();
            config.sources
        };
        let none = BTreeMap::new();
        for (case, vars, sources, stored, want) in [
            (
                "token_variable",
                &[(TOKEN_VAR, "ghp_x")][..],
                none.clone(),
                None,
                true,
            ),
            (
                "config_reference_env",
                &[("MY_TOKEN", "ghp_x")][..],
                reference("[sources.github]\ncredential = { env = \"MY_TOKEN\" }\n"),
                None,
                true,
            ),
            (
                "config_reference_env_unset",
                &[][..],
                reference("[sources.github]\ncredential = { env = \"MY_TOKEN\" }\n"),
                Some("ghp_stored\n"),
                false,
            ),
            (
                "stored_file",
                &[][..],
                none.clone(),
                Some("ghp_stored\n"),
                true,
            ),
            ("stored_file_empty", &[][..], none.clone(), Some(""), false),
            ("none", &[][..], none.clone(), None, false),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let paths = paths(dir.path());
            if let Some(text) = stored {
                store(&paths, text);
            }
            let lookup = |name: &str| {
                vars.iter()
                    // ubs:ignore — comparing environment variable names, not secrets
                    .find(|(k, _)| *k == name)
                    .map(|(_, v)| (*v).to_owned())
            };
            assert_eq!(
                GithubToken::present_with(&lookup, &paths, &sources),
                want,
                "{case}"
            );
        }
        // A configured file and a stored file are counted by their metadata alone: one
        // nobody can read still counts.
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        store(&paths, "ghp_stored\n");
        let file = paths.credential_file("github");
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o000)).unwrap();
        assert!(GithubToken::present_with(&|_| None, &paths, &none));
        let configured = reference(&format!(
            "[sources.github]\ncredential = {{ file = \"{}\" }}\n",
            file.display()
        ));
        assert!(GithubToken::present_with(&|_| None, &paths, &configured));
    }
}
