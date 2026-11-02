use std::path::PathBuf;

use super::super::Fixture;
use super::found_ids;
use crate::common::doctor::plant;
use crate::common::home::Home;
use crate::common::omarchy::stub_desk;

const ID: &str = "fm-plugins-widget-invalid";
const DRIFT: &str = "fm-plugins-widget-version-drift";

/// `fm-plugins-widget-invalid`: the installed bar widget breaks two of Omarchy's plugin
/// rules. This binary's widget is installed through the stub desk (`omarchy install`,
/// `PATH` holding only stub `omarchy-*` tools), then `plant::widget_invalid` sets
/// `barWidget.defaultSection` to `"top"` in its `manifest.json` and makes the symlink
/// `views/link → ../manifest.json` inside its folder; `Build.js` stays, so the widget is
/// not outdated. A hand edit or an Omarchy update that changed its rules leaves this.
///
/// The finding: P2, tier `quick`, about the machine (`fm-plugins-widget-invalid@machine`),
/// evidence `{path: <config>/omarchy/plugins/omaddons, count: 2, rules: ["default_section",
/// "symlink"]}` (never a validator message, which quotes the manifest), the remediation
/// argv `omaddons omarchy install`, never auto-fixable (M4-D50). The diagnosing run needs
/// `OMARCHY_PATH` set to `plant::omarchy_path(home)`, as every doctor suite's runs carry
/// it (`fixtures::found_ids` sets it). The remedy runs that `omarchy install` on the stub
/// desk, which replaces every file that differs from its render.
pub struct WidgetInvalid;

impl Fixture for WidgetInvalid {
    fn id(&self) -> &'static str {
        ID
    }

    fn corrupt(&self, home: &Home) {
        plant::widget_invalid(home);
    }

    /// Neither widget row is found.
    fn assert_healthy(&self, home: &Home) {
        let ids = found_ids(home, &["--only".to_owned(), format!("{ID},{DRIFT}")]);
        assert!(
            !ids.iter().any(|id| id == ID || id == DRIFT),
            "a widget row is still found: {ids:?}"
        );
    }

    fn subject(&self, _: &Home) -> Vec<PathBuf> {
        Vec::new()
    }

    fn remedy(&self, home: &Home) {
        stub_desk(&home.env).install(home);
    }
}
