//! Launcher configuration readers (vision §5.2 step 1b): Lutris, Heroic, Steam and
//! Bottles, native and Flatpak. Each reader is a small tolerant line or JSON reader
//! that extracts the keys naming Wine prefixes and client executables; there is no
//! general YAML or VDF parser.
//!
//! Readers open only the config files they name (Heroic and Steam directories also hold
//! account data), never write and never fail: a missing file is skipped silently, an
//! unreadable or malformed one becomes a diagnostic.

pub mod bottles;
pub mod heroic;
pub mod lutris;
pub mod steam;

use std::path::{Path, PathBuf};

use super::LauncherHint;

/// Where launchers keep their configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LauncherDirs {
    pub home: PathBuf,
    /// `$XDG_CONFIG_HOME`.
    pub config: PathBuf,
    /// `$XDG_DATA_HOME`.
    pub data: PathBuf,
}

impl LauncherDirs {
    /// `config` and `data` default to the XDG defaults under `home`.
    pub fn new(home: &Path, config: Option<&Path>, data: Option<&Path>) -> Self {
        Self {
            home: home.to_path_buf(),
            config: config.map_or_else(|| home.join(".config"), Path::to_path_buf),
            data: data.map_or_else(|| home.join(".local/share"), Path::to_path_buf),
        }
    }

    /// `<home>/.var/app/<app>`: a Flatpak's private home.
    fn flatpak(&self, app: &str) -> PathBuf {
        self.home.join(".var/app").join(app)
    }
}

/// Every reader's hints, in launcher order (Lutris, Heroic, Steam, Bottles), and the
/// diagnostics of files that could not be used.
pub fn collect_hints(dirs: &LauncherDirs) -> (Vec<LauncherHint>, Vec<String>) {
    let mut out = Hints::default();
    lutris::read(dirs, &mut out);
    heroic::read(dirs, &mut out);
    steam::read(dirs, &mut out);
    bottles::read(dirs, &mut out);
    (out.hints, out.diagnostics)
}

/// What the readers collect.
#[derive(Debug, Default)]
struct Hints {
    hints: Vec<LauncherHint>,
    diagnostics: Vec<String>,
}

impl Hints {
    /// A prefix hint, once per launcher and path.
    fn prefix(&mut self, launcher: &str, flatpak: bool, prefix: PathBuf) {
        self.push(LauncherHint {
            launcher: launcher.to_owned(),
            flatpak,
            prefix: Some(prefix),
            exe: None,
        });
    }

    /// An exe hint, once per launcher and path.
    fn exe(&mut self, launcher: &str, flatpak: bool, exe: PathBuf) {
        self.push(LauncherHint {
            launcher: launcher.to_owned(),
            flatpak,
            prefix: None,
            exe: Some(exe),
        });
    }

    fn push(&mut self, hint: LauncherHint) {
        if !self.hints.contains(&hint) {
            self.hints.push(hint);
        }
    }

    fn note(&mut self, note: String) {
        self.diagnostics.push(note);
    }
}

/// The value of a YAML-like `key: value` line when `key` is one of `keys`: surrounding
/// quotes are stripped (`''` and `\"` unescaped), and so is a trailing ` # comment`
/// after an unquoted value. A CR before the line end is ignored. `None` for other
/// keys and empty values.
pub fn yaml_value<'a>(line: &str, keys: &[&'a str]) -> Option<(&'a str, String)> {
    let (key, rest) = line.trim_end_matches('\r').split_once(':')?;
    let key = keys.iter().find(|k| **k == key.trim())?;
    let rest = rest.trim();
    let value = if let Some(quoted) = rest.strip_prefix('\'') {
        unquote(quoted, '\'')
    } else if let Some(quoted) = rest.strip_prefix('"') {
        unquote(quoted, '"')
    } else {
        let cut = rest.find(" #").unwrap_or(rest.len());
        rest.get(..cut).unwrap_or(rest).trim_end().to_owned()
    };
    (!value.is_empty()).then_some((key, value))
}

/// The text up to the closing `quote`: `''` is a quote inside single quotes and `\` escapes
/// the next character inside double quotes. An unclosed quote runs to the line end.
fn unquote(s: &str, quote: char) -> String {
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if quote == '"' => {
                if let Some(next) = chars.next() {
                    out.push(next);
                }
            }
            c if c == quote => {
                if quote == '\'' && chars.peek() == Some(&'\'') {
                    chars.next();
                    out.push('\'');
                } else {
                    break;
                }
            }
            c => out.push(c),
        }
    }
    out
}

/// A path from a launcher config: a leading `~`, `$HOME` or `${HOME}` is `home`; other
/// variables are left as they are.
pub fn expand_home(value: &str, home: &Path) -> PathBuf {
    for lead in ["~", "$HOME", "${HOME}"] {
        if let Some(rest) = value.strip_prefix(lead) {
            if rest.is_empty() {
                return home.to_path_buf();
            }
            if let Some(rest) = rest.strip_prefix('/') {
                return home.join(rest);
            }
        }
    }
    PathBuf::from(value)
}

/// The regular files in `dir` whose extension is `ext` (ignoring ASCII case), sorted;
/// none when `dir` cannot be listed.
fn files_with_ext(dir: &Path, ext: &str) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case(ext))
                && p.is_file()
        })
        .collect();
    files.sort();
    files
}

/// The text of a config file, or a diagnostic naming it.
fn read_config(launcher: &str, path: &Path, out: &mut Hints) -> Option<String> {
    let text = super::read_text(path);
    if text.is_none() {
        out.note(format!("{launcher}: cannot read {}", path.display()));
    }
    text
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn yaml_values() {
        let keys = &["exe", "prefix"];
        let v = |line: &str| yaml_value(line, keys);
        assert_eq!(v("  exe: /g/Wow.exe"), Some(("exe", "/g/Wow.exe".into())));
        assert_eq!(v("  exe: /g/Wow.exe\r"), Some(("exe", "/g/Wow.exe".into())));
        assert_eq!(
            v("prefix: '/g/pfx'  # the prefix"),
            Some(("prefix", "/g/pfx".into()))
        );
        assert_eq!(
            v("prefix: \"/g/a \\\"b\\\"\""),
            Some(("prefix", "/g/a \"b\"".into()))
        );
        assert_eq!(v("prefix: 'it''s'"), Some(("prefix", "it's".into())));
        assert_eq!(
            v("prefix: /g/a#b # note"),
            Some(("prefix", "/g/a#b".into()))
        );
        assert_eq!(v("prefix: C:/x"), Some(("prefix", "C:/x".into())));
        assert_eq!(v("prefix:"), None);
        assert_eq!(v("prefix: ''"), None);
        assert_eq!(v("prefix_command: x"), None);
        assert_eq!(v("no colon"), None);
    }

    #[test]
    fn home_expansion() {
        let home = Path::new("/h");
        assert_eq!(expand_home("~", home), PathBuf::from("/h"));
        assert_eq!(
            expand_home("~/Games/pfx", home),
            PathBuf::from("/h/Games/pfx")
        );
        assert_eq!(expand_home("$HOME/x", home), PathBuf::from("/h/x"));
        assert_eq!(expand_home("${HOME}/x", home), PathBuf::from("/h/x"));
        assert_eq!(expand_home("~other/x", home), PathBuf::from("~other/x"));
        assert_eq!(
            expand_home("$XDG_DATA_HOME/x", home),
            PathBuf::from("$XDG_DATA_HOME/x")
        );
        assert_eq!(expand_home("/abs", home), PathBuf::from("/abs"));
    }

    #[test]
    fn hints_are_kept_once() {
        let mut out = Hints::default();
        out.prefix("steam", false, PathBuf::from("/p"));
        out.prefix("steam", false, PathBuf::from("/p"));
        out.prefix("bottles", false, PathBuf::from("/p"));
        out.exe("lutris", false, PathBuf::from("/p/Wow.exe"));
        assert_eq!(out.hints.len(), 3);
    }

    proptest! {
        #[test]
        fn line_readers_take_any_text(text in any::<Vec<u8>>()) {
            let text = String::from_utf8_lossy(&text);
            for line in text.lines() {
                let _ = yaml_value(line, &["exe", "prefix", "Path"]);
                let _ = expand_home(line, Path::new("/h"));
            }
        }
    }
}
