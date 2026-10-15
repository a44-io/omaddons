//! Heroic: `GamesConfig/*.json` under `<config>/heroic`, native or Flatpak. Every string
//! under a `winePrefix` key, at any depth (each game's settings are an object named by
//! its app id), is a prefix.

use serde_json::Value;

use super::{Hints, LauncherDirs, expand_home, files_with_ext, read_config};

const LAUNCHER: &str = "heroic";
const FLATPAK: &str = "com.heroicgameslauncher.hgl";

pub(super) fn read(dirs: &LauncherDirs, out: &mut Hints) {
    let bases = [
        (false, dirs.config.join("heroic")),
        (true, dirs.flatpak(FLATPAK).join("config/heroic")),
    ];
    for (flatpak, base) in bases {
        for file in files_with_ext(&base.join("GamesConfig"), "json") {
            let Some(text) = read_config(LAUNCHER, &file, out) else {
                continue;
            };
            match wine_prefixes(&text) {
                Ok(prefixes) => {
                    for prefix in prefixes {
                        out.prefix(LAUNCHER, flatpak, expand_home(&prefix, &dirs.home));
                    }
                }
                Err(err) => out.note(format!(
                    "{LAUNCHER}: {} is not valid JSON ({err}); skipped",
                    file.display()
                )),
            }
        }
    }
}

/// The non-empty `winePrefix` strings in a GamesConfig file, depth first in key order.
pub fn wine_prefixes(text: &str) -> Result<Vec<String>, String> {
    let value: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let mut found = Vec::new();
    // (value, whether it sits under a `winePrefix` key)
    let mut stack = vec![(&value, false)];
    while let Some((v, prefix)) = stack.pop() {
        match v {
            Value::String(s) if prefix && !s.is_empty() => found.push(s.clone()),
            Value::Object(map) => {
                stack.extend(map.iter().rev().map(|(k, c)| (c, k == "winePrefix")));
            }
            Value::Array(items) => stack.extend(items.iter().rev().map(|c| (c, false))),
            _ => {}
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn nested_wine_prefixes() {
        let text = r#"{
            "wow": {"winePrefix": "/g/wow-pfx", "wineVersion": {"bin": "/usr/bin/wine"}},
            "other": {"winePrefix": "~/Games/Heroic/Prefixes/other", "nested": [{"winePrefix": "/g/deep"}]},
            "empty": {"winePrefix": ""},
            "version": "v0"
        }"#;
        assert_eq!(
            wine_prefixes(text).unwrap(),
            ["/g/deep", "~/Games/Heroic/Prefixes/other", "/g/wow-pfx"]
        );
    }

    #[test]
    fn invalid_json_is_an_error() {
        for text in ["", "{\"wow\": {\"winePrefix\": \"/g", "\u{fffd}\0"] {
            assert!(wine_prefixes(text).is_err(), "{text:?}");
        }
    }

    proptest! {
        #[test]
        fn any_text_is_handled(text in any::<Vec<u8>>()) {
            let _ = wine_prefixes(&String::from_utf8_lossy(&text));
        }
    }
}
