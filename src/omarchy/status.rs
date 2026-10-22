//! `omaddons omarchy [status]` (M3-D7, M3-D8): whether Omarchy and its shell are here,
//! and the installed widget as this run sees it: which binary and version it runs,
//! whether it is outdated against this binary, whether it serves this run's omaddons
//! state, where it sits on the bar and whether it validates. Read-only: it writes
//! nothing and makes no request; its one subprocess is the shell ping.

use schemars::JsonSchema;
use serde::Serialize;

use super::install::{self, PluginInfo};
use super::{shell, validate};
use crate::error::{CommandHint, Result};
use crate::state::paths::Paths;

/// Omarchy itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct OmarchyInfo {
    /// `<config-omarchy>` exists and Omarchy's shell is installed.
    pub present: bool,
    /// `omarchy-shell shell ping` answered `ok`.
    pub shell_running: bool,
    /// `omarchy-plugin-validate` is on `PATH`.
    pub validator: bool,
}

/// omaddons' port of Omarchy's validator over the installed widget.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Validation {
    pub ok: bool,
    pub errors: Vec<String>,
}

/// What `omarchy status` found.
#[derive(Debug, Clone)]
pub struct Report {
    pub omarchy: OmarchyInfo,
    pub plugin: PluginInfo,
    /// `None` when no widget is installed.
    pub validation: Option<Validation>,
    pub commands: Vec<CommandHint>,
}

/// Reads it all; see the module docs.
pub fn status() -> Result<Report> {
    let paths = Paths::from_env()?;
    let binary = install::current_binary()?;
    let plugin = install::describe(&paths, &binary);
    let omarchy = OmarchyInfo {
        present: install::present(&paths),
        shell_running: shell::ping(),
        validator: shell::on_path(shell::VALIDATOR),
    };
    let validation = plugin.installed.then(|| {
        let errors = validate::validate(&plugin.path);
        Validation {
            ok: errors.is_empty(),
            errors,
        }
    });
    Ok(Report {
        commands: recommendations(&plugin),
        omarchy,
        plugin,
        validation,
    })
}

/// `omarchy install` for an outdated widget, or for one installed but on no section of
/// the bar. Never for another context alone (M3-D7): reinstalling from this run would
/// point the widget at this run's omaddons state, which is the user's call.
fn recommendations(plugin: &PluginInfo) -> Vec<CommandHint> {
    let install = ["omaddons", "omarchy", "install"];
    if !plugin.installed {
        return Vec::new();
    }
    if plugin.outdated {
        return vec![CommandHint::new("reinstall_widget", install, false)];
    }
    if !plugin.enabled {
        return vec![CommandHint::new("enable_widget", install, false)];
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn plugin(installed: bool, outdated: bool, enabled: bool, context: bool) -> PluginInfo {
        PluginInfo {
            installed,
            path: PathBuf::from("/p/omaddons"),
            id: "a44.omaddons".into(),
            version: None,
            binary: None,
            outdated,
            enabled,
            section: None,
            context_matches: context,
        }
    }

    fn actions(plugin: &PluginInfo) -> Vec<String> {
        recommendations(plugin)
            .iter()
            .map(|hint| serde_json::to_value(hint).unwrap()["action"].to_string())
            .collect()
    }

    #[test]
    fn only_an_outdated_or_unplaced_widget_is_reinstalled() {
        assert_eq!(
            actions(&plugin(false, false, false, false)),
            Vec::<String>::new()
        );
        assert_eq!(
            actions(&plugin(true, true, true, true)),
            ["\"reinstall_widget\""]
        );
        assert_eq!(
            actions(&plugin(true, true, false, true)),
            ["\"reinstall_widget\""]
        );
        assert_eq!(
            actions(&plugin(true, false, false, true)),
            ["\"enable_widget\""]
        );
        assert_eq!(
            actions(&plugin(true, false, true, true)),
            Vec::<String>::new()
        );
        // Another omaddons state alone recommends nothing.
        assert_eq!(
            actions(&plugin(true, false, true, false)),
            Vec::<String>::new()
        );
    }
}
