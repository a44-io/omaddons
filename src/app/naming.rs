//! What a client is called where a person reads it: `display_name` in `status` and
//! `targets`. The id stays the handle `-t` takes; this is the name a player knows the
//! client by ("Classic Era", "WoW: Forever", "Wrath 3.3.5a private server"), chosen
//! here once so the widget never invents one (M3-D2).

use std::collections::BTreeMap;

use crate::limits::SOURCE_TEXT_MAX_CHARS;
use crate::model::TargetKey;
use crate::rules::{ClientVersion, Family, InstallKind, ModeName, TargetKind};
use crate::state::config::Config;
use crate::text::{Entities, source_line};

/// What naming a client reads.
#[derive(Debug, Clone, Copy)]
pub struct Client<'a> {
    pub id: &'a str,
    pub family: Family,
    pub kind: TargetKind,
    pub install_kind: InstallKind,
    /// The Battle.net product code, when known.
    pub product: Option<&'a str>,
    /// The product's game modes, primary first.
    pub modes: &'a [ModeName],
    pub version: ClientVersion,
    /// Whether the client's rules row is a legacy (private-server) row.
    pub legacy: bool,
    /// The name the player set in `config.toml` (`targets.<id or key>.name`), if any.
    pub chosen: Option<&'a str>,
}

/// The name the player set for a client in `[targets.<id or key>]`, by its id first, as
/// the other per-target overrides are found.
pub fn chosen<'a>(config: &'a Config, id: &str, key: Option<&TargetKey>) -> Option<&'a str> {
    let key = key.map(ToString::to_string);
    config
        .targets
        .get(id)
        .or_else(|| key.as_ref().and_then(|k| config.targets.get(k)))
        .and_then(|o| o.name.as_deref())
}

/// A client's display name, by one rule:
///
/// 1. a name the player set wins (plain text, cut to [`SOURCE_TEXT_MAX_CHARS`]; an empty
///    one restores the rule);
/// 2. a product with game modes takes its primary mode's name ("Retail", "Classic Era",
///    "Burning Crusade Anniversary");
/// 3. a Battle.net client without modes takes its family's name, and a test client adds
///    what it is ("Retail PTR", "Retail Beta", "Titan");
/// 4. a custom client takes its family's name and its version, with "private server"
///    when its rules row is legacy ("Wrath 3.3.5a private server");
/// 5. otherwise (a family omaddons does not know) the product code, else the id: nothing
///    is said that cannot be known.
pub fn display_name(client: &Client<'_>) -> String {
    if let Some(chosen) = client
        .chosen
        .map(|name| source_line(name, SOURCE_TEXT_MAX_CHARS, Entities::Keep))
        .filter(|name| !name.is_empty())
    {
        return chosen;
    }
    if let Some(mode) = client.modes.first() {
        return mode.name.clone();
    }
    let Some(family) = family_name(client.family) else {
        return client.product.unwrap_or(client.id).to_owned();
    };
    match client.install_kind {
        InstallKind::Battlenet => match client.kind {
            TargetKind::Live => family.to_owned(),
            TargetKind::Test => format!("{family} {}", test_label(client.product)),
        },
        InstallKind::Custom => {
            let version = version_text(client.version);
            if client.legacy {
                format!("{family} {version} private server")
            } else {
                format!("{family} {version}")
            }
        }
    }
}

/// `names` (each `(id, display name)`) with every name two clients share told apart by
/// the client's id: "Retail (ptr)". Names are compared ignoring case.
pub fn disambiguate(names: &mut [(String, String)]) {
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    for (_, name) in names.iter() {
        *seen.entry(name.to_lowercase()).or_default() += 1;
    }
    for (id, name) in names.iter_mut() {
        if seen
            .get(&name.to_lowercase())
            .is_some_and(|count| *count > 1)
        {
            *name = format!("{name} ({id})");
        }
    }
}

/// The name players use for a family's game, `None` for a family omaddons does not
/// know.
fn family_name(family: Family) -> Option<&'static str> {
    Some(match family {
        Family::Mainline => "Retail",
        Family::Vanilla => "Classic Era",
        Family::Forever => "WoW: Forever",
        Family::Tbc => "Burning Crusade Classic",
        Family::Wrath => "Wrath Classic",
        Family::Titan => "Titan",
        Family::Cata => "Cataclysm Classic",
        Family::Mists => "Mists of Pandaria Classic",
        Family::VanillaLegacy => "Vanilla",
        Family::TbcLegacy => "Burning Crusade",
        Family::WrathLegacy => "Wrath",
        Family::CataLegacy => "Cataclysm",
        Family::MistsLegacy => "Mists of Pandaria",
        Family::Unknown => return None,
    })
}

/// What a test product is, from its code: `XPTR`, `Beta`, else `PTR`.
fn test_label(product: Option<&str>) -> &'static str {
    let code = product.unwrap_or_default().to_ascii_lowercase();
    if code.contains("xptr") {
        "XPTR"
    } else if code.contains("beta") {
        "Beta"
    } else {
        "PTR"
    }
}

/// A client's version as players write it: `major.minor.patch`, and 3.3.5 as `3.3.5a`,
/// the name its build (12340) goes by.
fn version_text(version: ClientVersion) -> String {
    let text = format!("{}.{}.{}", version.major, version.minor, version.patch);
    if text == "3.3.5" {
        "3.3.5a".to_owned()
    } else {
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn modes(names: &[&str]) -> Vec<ModeName> {
        names
            .iter()
            .map(|name| ModeName {
                id: name.to_lowercase(),
                name: (*name).to_owned(),
            })
            .collect()
    }

    /// A table row's client: one argument per column of the case table.
    #[allow(clippy::too_many_arguments)]
    fn client<'a>(
        id: &'a str,
        family: Family,
        kind: TargetKind,
        install_kind: InstallKind,
        product: Option<&'a str>,
        modes: &'a [ModeName],
        version: ClientVersion,
        legacy: bool,
    ) -> Client<'a> {
        Client {
            id,
            family,
            kind,
            install_kind,
            product,
            modes,
            version,
            legacy,
            chosen: None,
        }
    }

    /// The development machine's six clients, and the rule's other branches.
    #[test]
    fn each_kind_of_client_gets_its_players_name() {
        use Family as F;
        use InstallKind::{Battlenet, Custom};
        use TargetKind::{Live, Test};
        let v = ClientVersion::new;
        let (retail, classic, era, anniversary, forever) = (
            modes(&["Retail"]),
            modes(&["Mists of Pandaria Classic"]),
            modes(&["Classic Era", "Hardcore", "Season of Discovery"]),
            modes(&["Burning Crusade Anniversary"]),
            modes(&["WoW: Forever"]),
        );
        let none: Vec<ModeName> = Vec::new();
        let cases = [
            (
                "retail",
                client(
                    "retail",
                    F::Mainline,
                    Live,
                    Battlenet,
                    Some("wow"),
                    &retail,
                    v(12, 1, 5),
                    false,
                ),
                "Retail",
            ),
            (
                "classic",
                client(
                    "classic",
                    F::Mists,
                    Live,
                    Battlenet,
                    Some("wow_classic"),
                    &classic,
                    v(5, 5, 4),
                    false,
                ),
                "Mists of Pandaria Classic",
            ),
            (
                "era",
                client(
                    "era",
                    F::Vanilla,
                    Live,
                    Battlenet,
                    Some("wow_classic_era"),
                    &era,
                    v(1, 15, 9),
                    false,
                ),
                "Classic Era",
            ),
            (
                "anniversary",
                client(
                    "anniversary",
                    F::Tbc,
                    Live,
                    Battlenet,
                    Some("wow_anniversary"),
                    &anniversary,
                    v(2, 5, 6),
                    false,
                ),
                "Burning Crusade Anniversary",
            ),
            (
                "forever",
                client(
                    "forever",
                    F::Forever,
                    Live,
                    Battlenet,
                    Some("wowf"),
                    &forever,
                    v(1, 60, 1),
                    false,
                ),
                "WoW: Forever",
            ),
            (
                "wotlk",
                client(
                    "wotlk",
                    F::WrathLegacy,
                    Live,
                    Custom,
                    None,
                    &none,
                    v(3, 3, 5),
                    true,
                ),
                "Wrath 3.3.5a private server",
            ),
            (
                "ptr",
                client(
                    "ptr",
                    F::Mainline,
                    Test,
                    Battlenet,
                    Some("wowt"),
                    &none,
                    v(12, 1, 7),
                    false,
                ),
                "Retail PTR",
            ),
            (
                "xptr",
                client(
                    "xptr",
                    F::Mainline,
                    Test,
                    Battlenet,
                    Some("wowxptr"),
                    &none,
                    v(12, 1, 7),
                    false,
                ),
                "Retail XPTR",
            ),
            (
                "beta",
                client(
                    "beta",
                    F::Mainline,
                    Test,
                    Battlenet,
                    Some("wow_beta"),
                    &none,
                    v(12, 2, 0),
                    false,
                ),
                "Retail Beta",
            ),
            (
                "titan",
                client(
                    "titan",
                    F::Titan,
                    Live,
                    Battlenet,
                    Some("wow_classic_titan"),
                    &none,
                    v(3, 80, 0),
                    false,
                ),
                "Titan",
            ),
            (
                "turtle",
                client(
                    "turtle",
                    F::VanillaLegacy,
                    Live,
                    Custom,
                    None,
                    &none,
                    v(1, 12, 1),
                    true,
                ),
                "Vanilla 1.12.1 private server",
            ),
            (
                "custom vanilla",
                client(
                    "mine",
                    F::Vanilla,
                    Live,
                    Custom,
                    None,
                    &none,
                    v(1, 15, 2),
                    false,
                ),
                "Classic Era 1.15.2",
            ),
            (
                "unknown battle.net",
                client(
                    "xyz",
                    F::Unknown,
                    Live,
                    Battlenet,
                    Some("wow_future"),
                    &none,
                    v(0, 0, 0),
                    false,
                ),
                "wow_future",
            ),
            (
                "unknown custom",
                client(
                    "my-server",
                    F::Unknown,
                    Live,
                    Custom,
                    None,
                    &none,
                    v(2, 6, 0),
                    false,
                ),
                "my-server",
            ),
        ];
        for (case, client, expected) in cases {
            assert_eq!(display_name(&client), expected, "{case}");
        }
    }

    #[test]
    fn a_chosen_name_wins_and_an_empty_one_restores_the_rule() {
        let retail = modes(&["Retail"]);
        let mut c = client(
            "retail",
            Family::Mainline,
            TargetKind::Live,
            InstallKind::Battlenet,
            Some("wow"),
            &retail,
            ClientVersion::new(12, 1, 5),
            false,
        );
        for (chosen, expected) in [
            (Some("Main"), "Main"),
            (Some("  my \u{1b}[31mmain\u{202e} "), "my [31mmain"),
            (Some(""), "Retail"),
            (Some("   "), "Retail"),
            (None, "Retail"),
        ] {
            c.chosen = chosen;
            assert_eq!(display_name(&c), expected, "{chosen:?}");
        }
        let too_long = "x".repeat(SOURCE_TEXT_MAX_CHARS + 50);
        c.chosen = Some(&too_long);
        let long = display_name(&c);
        assert!(long.chars().count() <= SOURCE_TEXT_MAX_CHARS + 1, "{long}");
    }

    #[test]
    fn shared_names_are_told_apart_by_id() {
        let mut names = vec![
            ("retail".to_owned(), "Retail".to_owned()),
            ("retail@steam".to_owned(), "retail".to_owned()),
            ("era".to_owned(), "Classic Era".to_owned()),
        ];
        disambiguate(&mut names);
        assert_eq!(
            names,
            [
                ("retail".to_owned(), "Retail (retail)".to_owned()),
                (
                    "retail@steam".to_owned(),
                    "retail (retail@steam)".to_owned()
                ),
                ("era".to_owned(), "Classic Era".to_owned()),
            ]
        );
    }
}
