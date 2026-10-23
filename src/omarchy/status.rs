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

/// The bar widget as `status` reports it (E4, M3-D7): whether it is installed, on the
/// bar, and from this build.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct WidgetInfo {
    /// Omarchy's shell is installed too, not only its config directory.
    pub omarchy: bool,
    pub installed: bool,
    /// It is in a section of the bar.
    pub enabled: bool,
    /// The installed manifest's `version`; `null` when none is installed.
    pub version: Option<String>,
    /// The installed code is not what this binary renders for the installed context:
    /// another build or other assets, never merely another context.
    pub outdated: bool,
    /// It was installed under this run's `HOME` and XDG directories; `null` when none is
    /// installed, or when its `Build.js` cannot say (it then serves no state at all).
    pub context_matches: Option<bool>,
}

impl WidgetInfo {
    /// `status` warns, raises the issue and recommends `omarchy install`: an outdated
    /// widget on an Omarchy desktop, unless it serves another omaddons state, which a
    /// reinstall from this run would point at this run's state instead (M3-D7).
    pub fn needs_reinstall(&self) -> bool {
        self.omarchy && self.installed && self.outdated && self.context_matches != Some(false)
    }
}

/// The bar widget for `status`: `None` without Omarchy's config directory. File reads
/// only, so `status --cached` keeps its budget.
pub fn widget(paths: &Paths) -> Option<WidgetInfo> {
    if !paths.omarchy_config_dir().is_dir() {
        return None;
    }
    let plugin = install::describe(paths);
    Some(WidgetInfo {
        omarchy: install::present(paths),
        installed: plugin.installed,
        enabled: plugin.installed && plugin.enabled,
        version: plugin.version,
        outdated: plugin.outdated,
        context_matches: plugin.context_known.then_some(plugin.context_matches),
    })
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
    let plugin = install::describe(&paths);
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
/// the bar. Never from another context (M3-D7), outdated or not: reinstalling from this
/// run would point the widget at this run's omaddons state, which is the user's call.
fn recommendations(plugin: &PluginInfo) -> Vec<CommandHint> {
    if !plugin.installed || plugin.context_known && !plugin.context_matches {
        return Vec::new();
    }
    if plugin.outdated {
        return vec![reinstall_hint()];
    }
    if !plugin.enabled {
        return vec![CommandHint::new(
            "enable_widget",
            ["omaddons", "omarchy", "install"],
            false,
        )];
    }
    Vec::new()
}

/// `omarchy install` for an outdated widget, as `omarchy status` and `status` recommend
/// it.
pub fn reinstall_hint() -> CommandHint {
    CommandHint::new(
        "reinstall_widget",
        ["omaddons", "omarchy", "install"],
        false,
    )
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
            context_known: installed,
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
        // Another omaddons state recommends nothing, even for an outdated widget.
        assert_eq!(
            actions(&plugin(true, false, true, false)),
            Vec::<String>::new()
        );
        assert_eq!(
            actions(&plugin(true, true, true, false)),
            Vec::<String>::new()
        );
        // A widget whose `Build.js` cannot say what it serves serves nothing.
        let unknown = PluginInfo {
            context_known: false,
            ..plugin(true, true, true, false)
        };
        assert_eq!(actions(&unknown), ["\"reinstall_widget\""]);
    }

    fn widget(omarchy: bool, installed: bool, outdated: bool, context: Option<bool>) -> WidgetInfo {
        WidgetInfo {
            omarchy,
            installed,
            enabled: installed,
            version: None,
            outdated,
            context_matches: context,
        }
    }

    #[test]
    fn only_an_outdated_widget_of_this_state_on_omarchy_asks_for_a_reinstall() {
        assert!(widget(true, true, true, Some(true)).needs_reinstall());
        assert!(!widget(true, true, false, Some(true)).needs_reinstall());
        assert!(!widget(true, true, true, Some(false)).needs_reinstall());
        assert!(!widget(false, true, true, Some(true)).needs_reinstall());
        assert!(!widget(true, false, false, None).needs_reinstall());
        assert!(
            widget(true, true, true, None).needs_reinstall(),
            "a widget serving no known state"
        );
    }
}
