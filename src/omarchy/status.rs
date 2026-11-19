//! `omaddons omarchy [status]` (M3-D7, M3-D8, omad-widget-plugin-3bcbh.10): whether
//! Omarchy and its shell are here, and the installed widget as this run sees it: a
//! checkout and its commit, or a widget an older omaddons rendered; whether it can serve
//! this binary; whether it serves this run's omaddons state; where it sits on the bar and
//! whether it validates. Read-only: it writes nothing and makes no request; its
//! subprocesses are the shell ping and, for the widget's checkout, its `state`.

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::Serialize;

use super::context::Context;
use super::install::{self, PluginInfo};
use super::{shell, validate};
use crate::contract::actions::Action;
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
    /// It cannot serve this binary as it is: a widget an older omaddons rendered (or its
    /// setup button), or a checkout whose manifest's minimum contract is not this
    /// binary's (omad-widget-plugin-3bcbh.10).
    pub outdated: bool,
    /// It serves this run's `HOME` and XDG directories, as a rendered widget's `Build.js`
    /// records them; `null` when none is installed, when its `Build.js` cannot say, and for
    /// the widget's checkout, whose desktop session only `omarchy status` asks (`status`
    /// starts no subprocess).
    pub context_matches: Option<bool>,
    /// The absolute path the widget runs this omaddons by: mise's `latest` link to it
    /// when mise installed it (so the widget follows mise's upgrades without its
    /// wrapper), else this binary's canonical path; `null` when this process cannot read
    /// its own path.
    pub binary: Option<PathBuf>,
    /// The absolute directory the doorbell rings in, as this process resolves its cache
    /// directory: every write that can change `status --cached` renames `doorbell.json`
    /// into it, so the widget watches it rather than polling.
    pub doorbell_dir: PathBuf,
    /// Why it is outdated (not on the wire).
    #[serde(skip)]
    pub stale: Option<install::Stale>,
    /// The folder is the widget's git checkout (not on the wire: `omarchy status` says).
    #[serde(skip)]
    pub checkout: bool,
    /// The checkout's minimum contract, as its manifest writes it (not on the wire).
    #[serde(skip)]
    pub contract_min: Option<String>,
}

impl WidgetInfo {
    /// `status` warns and raises the issue: an outdated widget on an Omarchy desktop,
    /// unless it serves another omaddons state, which a fix from this run would point at
    /// this run's state instead (M3-D7).
    pub fn outdated_here(&self) -> bool {
        self.omarchy && self.installed && self.outdated && self.context_matches != Some(false)
    }

    /// ... and recommends `omarchy install`: only for a widget an older omaddons rendered,
    /// which it replaces. A checkout of another contract is fixed by Omarchy's plugin
    /// update or an omaddons upgrade instead ([`WidgetInfo::fix`]).
    pub fn needs_reinstall(&self) -> bool {
        self.outdated_here() && self.stale == Some(install::Stale::Rendered)
    }

    /// The fix for an outdated widget: `omarchy install` for a rendered one; in words,
    /// Omarchy's plugin update for a checkout older than this binary's contract, or an
    /// omaddons upgrade for one that needs a later contract.
    pub fn fix(&self) -> crate::error::Suggestion {
        use crate::error::Suggestion;
        match self.stale {
            Some(install::Stale::WidgetOlder) => Suggestion::text(
                "update the widget through Omarchy: omarchy plugin update a44.omaddons",
            ),
            Some(install::Stale::OmaddonsOlder) => Suggestion::text(
                "upgrade omaddons to a build that speaks the contract the widget needs (with mise: mise upgrade github:a44-io/omaddons)",
            ),
            _ => Suggestion::command(["omaddons", "omarchy", "install"]),
        }
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
        binary: crate::binary::widget_binary().ok(),
        doorbell_dir: paths.doorbell_dir().to_path_buf(),
        stale: plugin.stale,
        checkout: plugin.checkout,
        contract_min: plugin.contract_min,
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
    let mut plugin = install::describe(&paths);
    let omarchy = OmarchyInfo {
        present: install::present(&paths),
        shell_running: shell::ping(),
        validator: shell::on_path(shell::VALIDATOR),
    };
    // The checkout serves its desktop session's directories: asked, when the shell runs
    // and the widget answers with them (omad-widget-plugin-3bcbh.10).
    if plugin.checkout
        && omarchy.shell_running
        && let Some(session) = shell::widget_session(&plugin.id)
    {
        plugin.context_matches = session == Context::of(&paths);
        plugin.context_known = true;
    }
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

/// `omarchy install` for a widget an older omaddons rendered, which it replaces, or for
/// one installed but on no section of the bar. Never from another context (M3-D7):
/// installing from this run would point the widget at this run's omaddons state, which
/// is the user's call. A checkout of another contract gets none: Omarchy's plugin update
/// or an omaddons upgrade fixes it, which the warning and the doctor say in words.
fn recommendations(plugin: &PluginInfo) -> Vec<CommandHint> {
    if !plugin.installed || plugin.context_known && !plugin.context_matches {
        return Vec::new();
    }
    if plugin.stale == Some(install::Stale::Rendered) {
        return vec![reinstall_hint()];
    }
    if plugin.outdated {
        return Vec::new();
    }
    if !plugin.enabled {
        return vec![CommandHint::new(
            Action::EnableWidget,
            ["omaddons", "omarchy", "install"],
        )];
    }
    Vec::new()
}

/// `omarchy install` for an outdated widget, as `omarchy status` and `status` recommend
/// it.
pub fn reinstall_hint() -> CommandHint {
    CommandHint::new(Action::ReinstallWidget, ["omaddons", "omarchy", "install"])
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
            checkout: false,
            commit: None,
            context_known: installed,
            stale: outdated.then_some(install::Stale::Rendered),
            contract_min: None,
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
        // The widget's checkout of another contract: omarchy install writes nothing under
        // it, so nothing is recommended, even off the bar; the warning says the fix.
        for stale in [install::Stale::WidgetOlder, install::Stale::OmaddonsOlder] {
            let checkout = PluginInfo {
                checkout: true,
                stale: Some(stale),
                ..plugin(true, true, false, true)
            };
            assert_eq!(actions(&checkout), Vec::<String>::new(), "{stale:?}");
        }
        // A current checkout off the bar is put on it.
        let current = PluginInfo {
            checkout: true,
            ..plugin(true, false, false, true)
        };
        assert_eq!(actions(&current), ["\"enable_widget\""]);
    }

    fn widget(omarchy: bool, installed: bool, outdated: bool, context: Option<bool>) -> WidgetInfo {
        WidgetInfo {
            omarchy,
            installed,
            enabled: installed,
            version: None,
            outdated,
            context_matches: context,
            binary: None,
            doorbell_dir: PathBuf::from("/cache/omaddons"),
            stale: outdated.then_some(install::Stale::Rendered),
            checkout: false,
            contract_min: None,
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

    /// A checkout of another contract is outdated here, with its own fix in words, and
    /// never asks for `omarchy install`, which writes nothing under it.
    #[test]
    fn a_checkout_of_another_contract_names_the_side_to_update() {
        let fix = |stale| {
            let w = WidgetInfo {
                stale: Some(stale),
                checkout: true,
                ..widget(true, true, true, None)
            };
            assert!(w.outdated_here() && !w.needs_reinstall(), "{stale:?}");
            w.fix().to_string()
        };
        assert!(fix(install::Stale::WidgetOlder).contains("omarchy plugin update a44.omaddons"));
        assert!(fix(install::Stale::OmaddonsOlder).contains("upgrade omaddons"));
        assert_eq!(
            widget(true, true, true, Some(true)).fix().to_string(),
            "omaddons omarchy install"
        );
    }
}
