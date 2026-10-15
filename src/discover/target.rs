//! What every discovery source produces: a target with no key yet, plus what the
//! registry needs to name it (vision §5.1, §5.2).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::model::Target;

/// A client found by discovery, before registration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct DiscoveredTarget {
    /// `key: None`; `id` is the default id suggestion.
    pub target: Target,
    /// The unqualified default id, e.g. `retail`.
    pub default_id: String,
    /// What the registry appends on an id collision, e.g. `battlenet` or `steam`.
    pub qualifier: String,
    pub source: DiscoverySource,
}

/// Where discovery learned of a client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DiscoverySource {
    /// Battle.net's `product.db`.
    ProductDb,
    /// A launcher's configuration, e.g. `lutris` or `steam`.
    Launcher { name: String },
    /// A running client process.
    Process,
    /// A root listed in config `extra_roots`.
    ExtraRoot,
    /// The bounded filesystem scan.
    Scan,
    /// `targets add`.
    Manual,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sources_serialize_with_a_kind() {
        let wire = |s: &DiscoverySource| serde_json::to_string(s).unwrap();
        assert_eq!(
            wire(&DiscoverySource::ProductDb),
            r#"{"kind":"product_db"}"#
        );
        assert_eq!(
            wire(&DiscoverySource::Launcher {
                name: "lutris".into()
            }),
            r#"{"kind":"launcher","name":"lutris"}"#
        );
        assert_eq!(
            wire(&DiscoverySource::ExtraRoot),
            r#"{"kind":"extra_root"}"#
        );
    }
}
