use std::path::PathBuf;

use super::super::Fixture;
use super::found_ids;
use crate::common::doctor::plant;
use crate::common::home::Home;
use crate::common::omarchy::stub_desk;

const ID: &str = "fm-plugins-widget-version-drift";

/// `fm-plugins-widget-version-drift`: the installed bar widget is from another omaddons
/// build. This binary's widget is installed through the stub desk (`omarchy install`,
/// `PATH` holding only stub `omarchy-*` tools), then `plant::widget_other_build` stamps
/// its `Build.js` with another code hash (64 zeros), as a widget another build rendered
/// carries. It still serves this state and still validates.
///
/// The finding: P3, tier `cached`, about the machine
/// (`fm-plugins-widget-version-drift@machine`), evidence `{path: <config>/omarchy/plugins/
/// omaddons, observed: <version>, expected: <version>, reason: code_differs}` (one version
/// on both sides: a rebuild), the remediation argv `omaddons omarchy install`, never
/// auto-fixable (M4-D50). `status` raises the same issue. The diagnosing run needs
/// `OMARCHY_PATH` set to `plant::omarchy_path(home)`, as every doctor suite's runs carry
/// it (`fixtures::found_ids` sets it). The remedy runs that `omarchy install` on the stub
/// desk.
pub struct WidgetVersionDrift;

impl Fixture for WidgetVersionDrift {
    fn id(&self) -> &'static str {
        ID
    }

    fn corrupt(&self, home: &Home) {
        plant::widget_other_build(home);
    }

    fn assert_healthy(&self, home: &Home) {
        let ids = found_ids(home, &["--only".to_owned(), ID.to_owned()]);
        assert!(!ids.iter().any(|id| id == ID), "{ID} still found: {ids:?}");
    }

    fn subject(&self, _: &Home) -> Vec<PathBuf> {
        Vec::new()
    }

    fn remedy(&self, home: &Home) {
        stub_desk(&home.env).install(home);
    }
}
