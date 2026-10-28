//! Enumerable facts of the CLI contract, shared by its descriptions and tests.

pub mod actions;
pub mod env;
pub mod events;
pub mod features;
pub mod schema;
pub mod volatile;

pub use volatile::VOLATILE;

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
