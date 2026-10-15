//! `WTF/Config.wtf`: the client's saved CVars, as `SET <name> "<value>"` lines.
//! Discovery reads `lastAddonVersion` (the interface the client last loaded addons
//! with) and, for custom clients, the realmlist.

use std::path::Path;

use super::{find_path, read_text};

/// `lastAddonVersion` from `<client dir>/WTF/Config.wtf`; `None` when the file, the
/// line or a numeric value is missing.
pub fn last_addon_version(client_dir: &Path) -> Option<u32> {
    cvar(client_dir, "lastAddonVersion")?.parse().ok()
}

/// The value of CVar `name` (ASCII case-insensitive) from `<client dir>/WTF/Config.wtf`.
pub fn cvar(client_dir: &Path, name: &str) -> Option<String> {
    let path = find_path(client_dir, &["WTF", "Config.wtf"])?;
    parse_cvar(&read_text(&path)?, name)
}

/// The value of the last `SET <name> <value>` line. Quotes around the value are
/// optional, and `SET` and the name ignore ASCII case.
pub(crate) fn parse_cvar(text: &str, name: &str) -> Option<String> {
    text.lines()
        .filter_map(|line| set_line(line, name))
        .next_back()
}

/// The value of one `SET <name> <value>` line, or `None` for any other line.
pub(crate) fn set_line(line: &str, name: &str) -> Option<String> {
    let line = line.trim();
    let (set, rest) = line.split_once(char::is_whitespace)?;
    if !set.eq_ignore_ascii_case("SET") {
        return None;
    }
    let rest = rest.trim_start();
    let (key, value) = rest.split_once(char::is_whitespace)?;
    if !key.eq_ignore_ascii_case(name) {
        return None;
    }
    let value = value.trim();
    let value = match value.strip_prefix('"') {
        Some(quoted) => quoted.split('"').next().unwrap_or_default(),
        None => value.split_whitespace().next().unwrap_or_default(),
    };
    Some(value.to_owned())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn set_lines() {
        for (text, expected) in [
            ("SET lastAddonVersion \"120105\"\n", Some("120105")),
            ("SET lastAddonVersion 120105\n", Some("120105")),
            ("set LASTADDONVERSION \"120105\"\r\n", Some("120105")),
            ("  SET   lastAddonVersion   \"120105\"   \n", Some("120105")),
            ("SET\tlastAddonVersion\t\"120105\"", Some("120105")),
            (
                "SET lastAddonVersion \"110000\"\nSET lastAddonVersion \"120105\"\n",
                Some("120105"),
            ),
            ("SET lastAddonVersionX \"1\"\n", None),
            ("SETlastAddonVersion \"1\"\n", None),
            ("SET lastAddonVersion\n", None),
            ("", None),
        ] {
            assert_eq!(
                parse_cvar(text, "lastAddonVersion").as_deref(),
                expected,
                "{text:?}"
            );
        }
        assert_eq!(
            parse_cvar("SET realmlist \"logon.example.org\"\n", "realmList").as_deref(),
            Some("logon.example.org")
        );
        assert_eq!(
            parse_cvar("SET portal \"\"\n", "portal").as_deref(),
            Some("")
        );
    }

    #[test]
    fn reads_config_wtf_from_the_client_dir() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(last_addon_version(dir.path()), None);
        fs::create_dir_all(dir.path().join("WTF")).unwrap();
        fs::write(
            dir.path().join("WTF").join("Config.wtf"),
            "SET locale \"enUS\"\nSET lastAddonVersion \"120100\"\n",
        )
        .unwrap();
        assert_eq!(last_addon_version(dir.path()), Some(120_100));
        assert_eq!(cvar(dir.path(), "locale").as_deref(), Some("enUS"));
        fs::write(
            dir.path().join("WTF").join("Config.wtf"),
            "SET lastAddonVersion \"next\"\n",
        )
        .unwrap();
        assert_eq!(last_addon_version(dir.path()), None);
    }
}
