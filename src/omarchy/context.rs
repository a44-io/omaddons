//! The data context of one omaddons run: exactly the inputs `Paths::from_env` reads, with
//! their defaults written out, so a child given them resolves the same `<config>`,
//! `<state>` and `<cache>` (M3-D7). The doctor's undo scripts run omaddons under it, and
//! a widget an older omaddons rendered recorded it in its `Build.js`
//! ([`super::legacy`]).

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::state::paths::Paths;

/// One run's effective roots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Context {
    pub home: PathBuf,
    pub xdg_config_home: PathBuf,
    pub xdg_state_home: PathBuf,
    pub xdg_cache_home: PathBuf,
    pub xdg_data_home: PathBuf,
}

impl Context {
    /// The roots `paths` was resolved from.
    pub fn of(paths: &Paths) -> Self {
        Self {
            home: paths.home().to_path_buf(),
            xdg_config_home: paths.config_home().to_path_buf(),
            xdg_state_home: paths.state_home().to_path_buf(),
            xdg_cache_home: paths.cache_home().to_path_buf(),
            xdg_data_home: paths.data_home().to_path_buf(),
        }
    }

    /// The five variables a child is given.
    pub fn as_env(&self) -> [(&'static str, &Path); 5] {
        [
            ("HOME", &self.home),
            ("XDG_CONFIG_HOME", &self.xdg_config_home),
            ("XDG_STATE_HOME", &self.xdg_state_home),
            ("XDG_CACHE_HOME", &self.xdg_cache_home),
            ("XDG_DATA_HOME", &self.xdg_data_home),
        ]
    }

    /// The context in a rendered widget's `CONTEXT` JSON: exactly the five names,
    /// absolute paths; `None` for anything else.
    pub(crate) fn from_json(text: &str) -> Option<Self> {
        let Value::Object(map) = serde_json::from_str::<Value>(text).ok()? else {
            return None;
        };
        if map.len() != 5 {
            return None;
        }
        let get = |name: &str| {
            map.get(name)
                .and_then(Value::as_str)
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
        };
        Some(Self {
            home: get("HOME")?,
            xdg_config_home: get("XDG_CONFIG_HOME")?,
            xdg_state_home: get("XDG_STATE_HOME")?,
            xdg_cache_home: get("XDG_CACHE_HOME")?,
            xdg_data_home: get("XDG_DATA_HOME")?,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::ffi::OsString;

    use super::*;

    /// A child given a run's context resolves the very same paths, defaults and
    /// fallbacks included.
    #[test]
    fn a_child_given_the_context_resolves_the_same_paths() {
        let lookup = |vars: Vec<(&str, OsString)>| {
            let vars: BTreeMap<String, OsString> =
                vars.into_iter().map(|(k, v)| (k.to_owned(), v)).collect();
            move |name: &str| vars.get(name).cloned()
        };
        for vars in [
            vec![("HOME", "/h".into())],
            vec![
                ("HOME", "/h".into()),
                ("XDG_CONFIG_HOME", "/xc".into()),
                ("XDG_STATE_HOME", "/xs".into()),
                ("XDG_CACHE_HOME", "/xk".into()),
                ("XDG_DATA_HOME", "/xd".into()),
            ],
            vec![("HOME", "/h".into()), ("XDG_STATE_HOME", "relative".into())],
        ] {
            let p = Paths::from_lookup(lookup(vars.clone())).unwrap();
            let context = Context::of(&p);
            let env: Vec<(&str, OsString)> = context
                .as_env()
                .iter()
                .map(|(name, path)| (*name, path.as_os_str().to_owned()))
                .collect();
            assert_eq!(Paths::from_lookup(lookup(env)).unwrap(), p, "{vars:?}");
        }
    }

    #[test]
    fn a_context_reads_from_exactly_five_absolute_paths() {
        let good = r#"{"HOME":"/h","XDG_CACHE_HOME":"/k","XDG_CONFIG_HOME":"/c","XDG_DATA_HOME":"/d","XDG_STATE_HOME":"/s"}"#;
        let read = Context::from_json(good).unwrap();
        assert_eq!((read.home, read.xdg_cache_home), ("/h".into(), "/k".into()));
        for bad in [
            r#"{"HOME":"relative","XDG_CACHE_HOME":"/c","XDG_CONFIG_HOME":"/c","XDG_DATA_HOME":"/d","XDG_STATE_HOME":"/s"}"#,
            r#"{"XDG_CACHE_HOME":"/c","XDG_CONFIG_HOME":"/c","XDG_DATA_HOME":"/d","XDG_STATE_HOME":"/s"}"#,
            r#"{"EXTRA":"/e","HOME":"/h","XDG_CACHE_HOME":"/c","XDG_CONFIG_HOME":"/c","XDG_DATA_HOME":"/d","XDG_STATE_HOME":"/s"}"#,
            "[]",
            "not json",
        ] {
            assert_eq!(Context::from_json(bad), None, "{bad}");
        }
    }
}
