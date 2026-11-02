use std::path::PathBuf;

use super::super::Fixture;
use super::assert_online_clean;
use crate::common::doctor::probes;
use crate::common::home::Home;

const ID: &str = "fm-network-github-rate-limited";

/// When the core budget resets in the recorded answers: 2026-01-01T00:00:00Z.
pub const BUDGET_RESETS_AT: i64 = 1_767_225_600;

/// `fm-network-github-rate-limited`: GitHub's `/rate_limit` answers that 2 of 60 core
/// REST requests remain this hour, without a token. A spent budget is GitHub's state, so
/// the fixture is the recorded answer (`probes::rate_limit(home, 60, 2, BUDGET_RESETS_AT)`), never a
/// file in the home.
///
/// The finding: P2, about the machine (`@machine`), evidence `{rate_limit: {limit: 60,
/// remaining: 2, reset_at: "2026-01-01T00:00:00Z"}, authenticated: false}`, the advice to
/// wait for the reset or store a token. Online only: the diagnosing run adds `--online`,
/// and appends one `doctor-probe` line to `net.log` per GitHub row, which F9's tree
/// comparisons exclude (§6.15). The remedy is GitHub's own: the budget reset (59 left).
pub struct GithubRateLimited;

impl Fixture for GithubRateLimited {
    fn id(&self) -> &'static str {
        ID
    }

    fn corrupt(&self, home: &Home) {
        probes::rate_limit(home, 60, 2, BUDGET_RESETS_AT);
    }

    fn assert_healthy(&self, home: &Home) {
        assert_online_clean(home, ID);
    }

    fn subject(&self, _: &Home) -> Vec<PathBuf> {
        Vec::new()
    }

    fn remedy(&self, home: &Home) {
        probes::rate_limit(home, 60, 59, BUDGET_RESETS_AT);
    }

    fn run_args(&self) -> Vec<String> {
        vec!["--online".to_owned()]
    }
}
