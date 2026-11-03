//! Enumerable facts of the CLI contract, shared by its descriptions and tests.

pub mod actions;
pub mod commands;
pub mod effective;
pub mod env;
pub mod events;
pub mod features;
pub mod guide;
pub mod manifest;
pub mod opaque;
pub mod schema;
pub mod values;
pub mod volatile;
pub mod wire;

use std::collections::BTreeMap;

pub use opaque::{OPAQUE, Opaque};
pub use volatile::VOLATILE;

use crate::error::Warning;
use crate::sources::SourceId;
use crate::sources::github_token::GithubToken;
use crate::state::config::{self, Config, ConfigProblem, SourceConfig, TolerantConfig};
use crate::state::paths::Paths;

/// The contract registry: what every description of the CLI reads (M4-D2): the commands
/// from the live clap tree joined with their `COMMANDS` rows, and the values `config.toml`
/// can change, as a run would use them.
///
/// Two ways to get one. [`Registry::builtin`] is the binary alone: the manifest, the guide
/// and `robot-docs` read it, so they never vary with a user's config. [`Registry::build`]
/// adds this machine's `config.toml` and which credentials are present, for
/// `capabilities` (M4-D4), and never fails: a config it cannot use leaves the built-in
/// values in place and says why in `config_error`.
#[derive(Debug, Clone, PartialEq)]
pub struct Registry {
    pub commands: Vec<commands::CommandInfo>,
    /// Every limit, source, family range and client-rules row, as in effect.
    pub effective: effective::Effective,
    /// Why `config.toml` could not be used; the built-in values stand in.
    pub config_error: Option<ConfigProblem>,
    /// `config.toml`'s `config_unknown_key` warnings.
    pub warnings: Vec<Warning>,
}

impl Registry {
    /// The binary alone: no I/O and no environment at all.
    pub fn builtin() -> Self {
        Self {
            commands: commands::commands(),
            effective: effective::Effective::builtin(),
            config_error: None,
            warnings: Vec::new(),
        }
    }

    /// The binary with this machine's `config.toml`, read through the process environment,
    /// and the credentials present (never read, never run). Never fails: without a usable
    /// `HOME` there is no config path, and that is the `config_error`.
    pub fn build() -> Self {
        match Paths::from_env() {
            Ok(paths) => {
                let config = Config::load_tolerant(&paths);
                let sources = config.loaded.config.sources.clone();
                Self::from_parts(config, &|id| credential_present(id, &paths, &sources))
            }
            Err(error) => Self {
                config_error: Some(ConfigProblem::unlocated(None, error.message.clone())),
                ..Self::builtin()
            },
        }
    }

    /// The registry of a loaded `config`, `credential` saying which sources have a
    /// credential. A config with a problem gives the built-in values, every one `builtin`,
    /// and no warnings; a credential found outside it (the variable, the stored token) is
    /// still reported, because the config being broken does not unset it.
    pub fn from_parts(config: TolerantConfig, credential: &dyn Fn(SourceId) -> bool) -> Self {
        let commands = commands::commands();
        match config.problem {
            Some(problem) => Self {
                commands,
                effective: effective::Effective::of(&Config::default(), credential),
                config_error: Some(problem),
                warnings: Vec::new(),
            },
            None => Self {
                commands,
                effective: effective::Effective::of(&config.loaded.config, credential),
                config_error: None,
                warnings: config.loaded.warnings,
            },
        }
    }
}

/// Whether `id` has a credential, by presence alone (M4-D4): GitHub in its token's
/// resolution order; CurseForge and Wago by their configured reference; no other source
/// takes one.
fn credential_present(
    id: SourceId,
    paths: &Paths,
    sources: &BTreeMap<String, SourceConfig>,
) -> bool {
    match id {
        SourceId::Github => GithubToken::present(paths, sources),
        SourceId::Curseforge | SourceId::Wago => sources
            .get(id.wire())
            .and_then(|source| source.credential.as_ref())
            .is_some_and(config::credential_present),
        _ => false,
    }
}

/// The forms `-t` accepts, in selector precedence, followed by the `all` keyword.
pub fn target_id_forms() -> Vec<&'static str> {
    crate::discover::registry::SelectorForm::ALL
        .iter()
        .map(|f| f.wire())
        .collect()
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::contract::effective::ValueSource;
    use crate::state::config::LoadedConfig;

    /// `text` loaded as config.toml, with whatever problem `problem` names.
    fn tolerant(text: &str, problem: Option<ConfigProblem>) -> TolerantConfig {
        let (config, warnings) = Config::parse(text, Path::new("/x/config.toml")).unwrap();
        TolerantConfig {
            loaded: LoadedConfig {
                config,
                warnings,
                path: PathBuf::from("/x/config.toml"),
                exists: true,
                text: Some(text.to_owned()),
            },
            problem,
        }
    }

    #[test]
    fn registry_builtin_needs_no_environment() {
        let builtin = Registry::builtin();
        assert_eq!(
            builtin,
            Registry::from_parts(TolerantConfig::default_for_tests(), &|_| false)
        );
        assert_eq!(builtin.config_error, None);
        assert!(builtin.warnings.is_empty());
    }

    #[test]
    fn from_parts_uses_the_config_and_its_warnings() {
        let registry = Registry::from_parts(
            tolerant("nope = 1\n[limits]\ndownload_max_bytes = \"1GiB\"\n", None),
            &|_| false,
        );
        assert_eq!(registry.config_error, None);
        assert_eq!(registry.warnings.len(), 1, "{:?}", registry.warnings);
        let download = registry
            .effective
            .limits
            .iter()
            .find(|limit| limit.info.name == "DOWNLOAD_MAX_BYTES")
            .unwrap();
        assert_eq!(download.source, ValueSource::Config);
    }

    #[test]
    fn from_parts_uses_builtin_values_with_a_problem() {
        let problem = ConfigProblem::unlocated(Some(PathBuf::from("/x/config.toml")), "broken");
        let registry = Registry::from_parts(
            tolerant(
                "nope = 1\ngit_hosts = [\"codeberg.org\"]\n[limits]\ndownload_max_bytes = \"1GiB\"\n",
                Some(problem.clone()),
            ),
            // ubs:ignore — comparing source ids, not secrets
            &|id| id == SourceId::Github,
        );
        assert_eq!(registry.config_error, Some(problem));
        assert!(registry.warnings.is_empty(), "{:?}", registry.warnings);
        let builtin = effective::Effective::builtin();
        assert_eq!(registry.effective.limits, builtin.limits);
        assert_eq!(registry.effective.families, builtin.families);
        assert_eq!(registry.effective.client_rules, builtin.client_rules);
        for source in &registry.effective.sources {
            assert_eq!(source.source, ValueSource::Builtin, "{:?}", source.id);
            // A token found outside the broken file is still there.
            assert_eq!(
                source.credential_configured,
                // ubs:ignore — comparing source ids, not secrets
                source.id == SourceId::Github,
                "{:?}",
                source.id
            );
        }
    }

    #[test]
    fn target_id_forms_are_the_selector_forms() {
        assert_eq!(
            super::target_id_forms(),
            [
                "key",
                "id",
                "product_code",
                "flavor_folder",
                "game_mode",
                "family",
                "path",
                "all"
            ]
        );
    }
}
