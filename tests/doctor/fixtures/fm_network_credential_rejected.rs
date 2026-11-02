use std::path::PathBuf;

use super::super::Fixture;
use super::assert_online_clean;
use crate::common::doctor::{plant, probes};
use crate::common::home::Home;

const ID: &str = "fm-network-credential-rejected";

/// The fake token the fixture stores: never a real one.
// ubs:ignore — a fake token for tests, never a real one
pub const TOKEN: &str = "ghp_F7ONLINE00000000000000000000000000000";

/// `fm-network-credential-rejected`: a token stored as `auth github set` leaves it
/// (`<config>/omaddons/credentials/github`, mode 0600, written by hand), which GitHub's
/// `/rate_limit` answers 401 (`probes::rate_limit_status(home, 401, &[], None)`).
///
/// The finding: P1, about the machine, evidence `{reason: rejected, http_status: 401,
/// origin: file, source: github}`, the remediation argv `omaddons auth github clear`. The
/// remedy runs that argv: the file is emptied, an empty file is no token, and the row
/// then sends no request at all. Online only: the diagnosing run adds `--online`, and
/// appends `doctor-probe` lines to `net.log`, which F9's tree comparisons exclude (§6.15).
pub struct CredentialRejected;

impl Fixture for CredentialRejected {
    fn id(&self) -> &'static str {
        ID
    }

    fn corrupt(&self, home: &Home) {
        plant::stored_token(home, TOKEN);
        probes::rate_limit_status(home, 401, &[], None);
    }

    /// No finding, and no request: no credential is configured once it is cleared.
    fn assert_healthy(&self, home: &Home) {
        let added = assert_online_clean(home, ID);
        assert_eq!(added, 0, "{ID} sends nothing without a credential");
    }

    fn subject(&self, _: &Home) -> Vec<PathBuf> {
        Vec::new()
    }

    fn remedy(&self, home: &Home) {
        let run = home.run(&["auth", "github", "clear", "--json"]);
        assert_eq!(run.code, 0, "auth github clear\n{}", run.context());
    }

    fn run_args(&self) -> Vec<String> {
        vec!["--online".to_owned()]
    }
}
