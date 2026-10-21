//! The privacy check (M3-D21, every M3 test that creates `WTF/`): SavedVariables under
//! fake account, realm and character names, and assertions that no output names them,
//! on stdout and stderr alike. Run commands with `--progress json` so progress lines are
//! checked too.

/// The account-wide SavedVariables folder, relative to the client dir.
pub const ACCOUNT: &str = "WTF/Account/FAKEACCT/SavedVariables";
/// One character's SavedVariables folder, relative to the client dir.
pub const CHARACTER: &str = "WTF/Account/FAKEACCT/FakeRealm/FakeChar/SavedVariables";
/// The names no output may contain, in any case.
pub const PRIVATE: &[&str] = &["FAKEACCT", "FakeRealm", "FakeChar"];

/// `output` names no account, realm or character of [`PRIVATE`], in any case.
pub fn assert_private(output: &str) {
    let folded = output.to_lowercase();
    for name in PRIVATE {
        assert!(
            !folded.contains(&name.to_lowercase()),
            "{name} in output:\n{output}"
        );
    }
}

/// [`assert_private`] over a run's stdout and stderr.
pub fn assert_private_all(stdout: &str, stderr: &str) {
    assert_private(stdout);
    assert_private(stderr);
}
