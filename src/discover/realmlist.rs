//! A custom client's realmlist host, which names the target in labels: `127.0.0.1`
//! becomes `localhost`. Only `Data/<locale>/realmlist.wtf` is a verified location
//! (clients 3.0.2–5.0.4); `<root>/realmlist.wtf` (older clients) and `Config.wtf`'s
//! `realmList` CVar (newer ones) are unverified best-effort lookups.

use std::path::Path;

use super::{config_wtf, find_child, read_text};

/// The realmlist host: the first `Data/<locale>/realmlist.wtf` with a `set realmlist`
/// line (locales sorted), else `<root>/realmlist.wtf`, else `WTF/Config.wtf`.
pub fn realmlist_host(root: &Path) -> Option<String> {
    let in_file = |path: &Path| read_text(path).and_then(|text| parse_realmlist(&text));
    if let Some(data) = find_child(root, "Data")
        && let Ok(entries) = std::fs::read_dir(data)
    {
        let mut locales: Vec<_> = entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        locales.sort();
        if let Some(host) = locales
            .iter()
            .filter_map(|locale| find_child(locale, "realmlist.wtf"))
            .find_map(|path| in_file(&path))
        {
            return Some(host);
        }
    }
    find_child(root, "realmlist.wtf")
        .and_then(|path| in_file(&path))
        .or_else(|| config_wtf::cvar(root, "realmList").filter(|h| !h.is_empty()))
}

/// The host of the first `set realmlist <host>` line (case-insensitive, quotes
/// optional); `None` without one or when it is empty.
pub(crate) fn parse_realmlist(text: &str) -> Option<String> {
    text.lines()
        .filter_map(|line| config_wtf::set_line(line, "realmlist"))
        .find(|host| !host.is_empty())
}

/// A target label: loopback hosts read `localhost`, any other host is verbatim, and
/// without a host the root directory's name is used.
pub fn label_for(host: Option<&str>, root: &Path) -> String {
    match host {
        Some(host) if is_loopback(host) => "localhost".to_owned(),
        Some(host) => host.to_owned(),
        None => root
            .file_name()
            .map_or_else(|| "custom".to_owned(), |n| n.to_string_lossy().into_owned()),
    }
}

/// `127.0.0.1`, `localhost` or `::1`, with or without a `:<port>`.
fn is_loopback(host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    let bare = match host.rsplit_once(':') {
        Some((name, port)) if !name.contains(':') && port.bytes().all(|b| b.is_ascii_digit()) => {
            name
        }
        _ => host.as_str(),
    };
    matches!(bare, "127.0.0.1" | "localhost" | "::1" | "[::1]")
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn realmlist_lines() {
        for (text, expected) in [
            ("set realmlist 127.0.0.1\n", Some("127.0.0.1")),
            (
                "SET realmList \"logon.example.org\"\r\n",
                Some("logon.example.org"),
            ),
            (
                "  set   REALMLIST   logon.example.org   \n",
                Some("logon.example.org"),
            ),
            (
                "set patchlist x\nset realmlist a\nset realmlist b\n",
                Some("a"),
            ),
            ("set realmlist \"\"\nset realmlist b\n", Some("b")),
            ("set realmlistx a\n", None),
            ("", None),
        ] {
            assert_eq!(parse_realmlist(text).as_deref(), expected, "{text:?}");
        }
    }

    #[test]
    fn labels() {
        let root = PathBuf::from("/games/My Server");
        for (host, expected) in [
            (Some("127.0.0.1"), "localhost"),
            (Some("LOCALHOST"), "localhost"),
            (Some("::1"), "localhost"),
            (Some("127.0.0.1:3724"), "localhost"),
            (Some("logon.example.org"), "logon.example.org"),
            (Some("logon.example.org:3724"), "logon.example.org:3724"),
            (Some("127.0.0.2"), "127.0.0.2"),
            (None, "My Server"),
        ] {
            assert_eq!(label_for(host, &root), expected, "{host:?}");
        }
        assert_eq!(label_for(None, Path::new("/")), "custom");
    }

    #[test]
    fn realmlist_locations_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        assert_eq!(realmlist_host(root), None);

        fs::create_dir_all(root.join("WTF")).unwrap();
        fs::write(
            root.join("WTF/Config.wtf"),
            "SET realmList \"config.example.org\"\n",
        )
        .unwrap();
        assert_eq!(realmlist_host(root).as_deref(), Some("config.example.org"));

        fs::write(
            root.join("realmlist.wtf"),
            "set realmlist root.example.org\n",
        )
        .unwrap();
        assert_eq!(realmlist_host(root).as_deref(), Some("root.example.org"));

        fs::create_dir_all(root.join("data/enGB")).unwrap();
        fs::create_dir_all(root.join("data/enUS")).unwrap();
        fs::write(
            root.join("data/enUS/REALMLIST.WTF"),
            "set realmlist 127.0.0.1\r\n",
        )
        .unwrap();
        assert_eq!(
            realmlist_host(root).as_deref(),
            Some("127.0.0.1"),
            "case-insensitive names"
        );
        fs::write(
            root.join("data/enGB/realmlist.wtf"),
            "set realmlist gb.example.org\n",
        )
        .unwrap();
        assert_eq!(
            realmlist_host(root).as_deref(),
            Some("gb.example.org"),
            "locales sorted"
        );
    }
}
