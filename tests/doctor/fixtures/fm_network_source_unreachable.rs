use std::path::PathBuf;

use omaddons::sources::SourceId;

use super::super::Fixture;
use super::assert_online_clean;
use crate::common::doctor::{plant, probes};
use crate::common::home::Home;

const ID: &str = "fm-network-source-unreachable";

/// `fm-network-source-unreachable`: retail holds a GitHub and a WoWInterface addon
/// (`plant::records_for_sources`, installed offline from local archives), GitHub's probe
/// answers its usual 302, and WoWInterface's `filedetails` answers 503.
///
/// The finding: one instance, `fm-network-source-unreachable@machine:source=wowi`
/// (label `machine · WoWInterface`), P2, confidence `high`, evidence `{reason:
/// server_error, source: wowi, host: "api.mmoui.com", http_status: 503}`, advice only.
/// GitHub answered, so it is not reported. The remedy is the source's own: it answers 200
/// again. Online only: the diagnosing run adds `--online`, and appends one `doctor-probe`
/// line per probed source (and per GitHub row) to `net.log`, which F9's tree comparisons
/// exclude (§6.15).
pub struct SourceUnreachable;

impl Fixture for SourceUnreachable {
    fn id(&self) -> &'static str {
        ID
    }

    fn corrupt(&self, home: &Home) {
        plant::records_for_sources(home, &[SourceId::Github, SourceId::Wowi]);
        probes::source(home, SourceId::Github, 302);
        probes::source(home, SourceId::Wowi, 503);
    }

    fn assert_healthy(&self, home: &Home) {
        assert_online_clean(home, ID);
    }

    fn subject(&self, _: &Home) -> Vec<PathBuf> {
        Vec::new()
    }

    fn remedy(&self, home: &Home) {
        probes::source(home, SourceId::Wowi, 200);
    }

    fn run_args(&self) -> Vec<String> {
        vec!["--online".to_owned()]
    }
}
