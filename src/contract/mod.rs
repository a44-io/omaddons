//! Enumerable facts of the CLI contract, shared by its descriptions and tests.

pub mod actions;
pub mod commands;
pub mod env;
pub mod events;
pub mod features;
pub mod guide;
pub mod manifest;
pub mod opaque;
pub mod schema;
pub mod volatile;
pub mod wire;

pub use opaque::{OPAQUE, Opaque};
pub use volatile::VOLATILE;

/// The contract registry: what every description of the CLI reads (M4-D2). A4
/// (`capabilities`) adds `build()` with the user's `config.toml`, and the parts beyond the
/// commands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registry {
    pub commands: Vec<commands::CommandInfo>,
}

impl Registry {
    /// The binary alone: no I/O and no environment at all. The manifest, the guide and
    /// `robot-docs` read it.
    pub fn builtin() -> Self {
        Self {
            commands: commands::commands(),
        }
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
