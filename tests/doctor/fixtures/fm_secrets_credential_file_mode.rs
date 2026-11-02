use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use super::super::Fixture;
use super::found_ids;
use crate::common::doctor::plant;
use crate::common::home::Home;

// ubs:ignore — a failure-mode id, not a secret
const ID: &str = "fm-secrets-credential-file-mode";

/// `fm-secrets-credential-file-mode`: the stored GitHub token is readable by other users.
/// `plant::credential_file_mode` writes a fake token (`plant::MODE_TOKEN`) to
/// `<config>/omaddons/credentials/github` as `auth github set` would, in a 0700 directory,
/// then sets the file to 0644 by hand (a copy that lost its mode, or a umask).
///
/// The finding: P2, tier `cached`, about the machine
/// (`fm-secrets-credential-file-mode@machine:path=<the file>`), title "The GitHub token
/// file is readable by other users", evidence `{path, expected: "0600", mode: "0644",
/// origin: file, source: github}` (judged from a stat: the file is never read), and the
/// remediation `omaddons doctor fix --only fm-secrets-credential-file-mode`, auto-fixable.
/// Its fixer chmods the file to 0600 in place (same inode, same bytes, no backup); the
/// subject is that one file, whose mode a round trip compares: undoing the repair
/// restores 0644, and with it the exposure.
pub struct CredentialFileMode;

impl Fixture for CredentialFileMode {
    fn id(&self) -> &'static str {
        ID
    }

    fn corrupt(&self, home: &Home) {
        plant::credential_file_mode(home);
    }

    /// The token file is 0600, and no instance is found.
    fn assert_healthy(&self, home: &Home) {
        let file = plant::token_file(home);
        let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o7777;
        assert_eq!(mode, 0o600, "{}: {mode:04o}", file.display());
        let ids = found_ids(home, &["--only".to_owned(), ID.to_owned()]);
        assert!(!ids.iter().any(|id| id == ID), "{ID} still found: {ids:?}");
    }

    fn subject(&self, home: &Home) -> Vec<PathBuf> {
        vec![plant::token_file(home)]
    }

    fn fixer(&self) -> bool {
        true
    }
}
