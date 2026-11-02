//! The chmod fixer of `fm-secrets-credential-file-mode` (M4-D51): an exposed credential
//! file inside omaddons' config directory set to 0600, as `auth github set` writes it,
//! through E5's repair transaction. It stats, never opens: the file's bytes are never
//! read, and a chmod keeps no backup. Undoing the repair restores the old mode, and with
//! it the exposure, as the row's `why` says before anyone runs it.

use crate::doctor::detectors::secrets::{credential_path, exposed_mode, fixability};
use crate::doctor::finding::Finding;
use crate::doctor::registry::{Cardinality, FixerPrecondition, FixerSpec, Refusal, ScopeName};
use crate::doctor::view::DoctorView;
use crate::engine::repair_action::{Fix, FixKind, Mode};

/// `auth github set`'s mode: a module constant, as backup's file modes are.
const CREDENTIAL_MODE: u32 = 0o600;

/// The fixer's declaration: enabled (a mode is no user content), one chmod under the
/// lock, inside `<config>` only.
pub const SPEC: FixerSpec = FixerSpec {
    plan,
    writes_to: &[ScopeName::Config],
    ops: &[FixKind::Chmod],
    enabled: true,
    needs_force: false,
    cardinality: Cardinality::Low,
    preconditions: &[FixerPrecondition::LockHeld],
};

/// One `Fix::Chmod` to 0600 for the finding's credential while the Credentials layer
/// still shows it exposed and the doctor can repair it; nothing for an instance that is
/// manual (a symlink, a file outside `<config>`, another user's), fixed since, or gone,
/// which `doctor fix` then lists as manual. Never a refusal: one manual instance must
/// not block the repairable ones of the same run.
pub fn plan(view: &DoctorView, finding: &Finding) -> Result<Vec<Fix>, Refusal> {
    let (Some(path), Some(credentials), Some(mode)) = (
        finding.evidence.path.as_deref(),
        view.credentials(),
        Mode::new(CREDENTIAL_MODE),
    ) else {
        return Ok(Vec::new());
    };
    let repairable = credentials.files.iter().find(|stat| {
        // ubs:ignore — two paths, never a secret's bytes
        credential_path(stat) == path
            && exposed_mode(stat).is_some()
            && fixability(view.paths(), stat).is_ok()
    });
    Ok(repairable
        .map(|_| Fix::Chmod {
            path: path.to_path_buf(),
            mode,
        })
        .into_iter()
        .collect())
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::doctor::budget::Budget;
    use crate::doctor::detectors::secrets::credential_file_mode;
    use crate::doctor::detectors::secrets::tests::{view_of, write};
    use crate::doctor::finding::{Confidence, Evidence, Observation, Subject};
    use crate::doctor::registry::{self, ids, invariant_violations};
    use crate::doctor::selection::Selection;
    use crate::doctor::view::tests::home;
    use crate::doctor::view::{CredentialOrigin, CredentialStat, FileStat};

    fn me() -> u32 {
        rustix::process::geteuid().as_raw()
    }

    fn stat(path: PathBuf, mode: u32, uid: u32, symlink: bool) -> CredentialStat {
        CredentialStat {
            path,
            origin: CredentialOrigin::Stored,
            stat: FileStat::Regular {
                mode,
                uid,
                bytes: 6,
                symlink,
            },
        }
    }

    /// The row's finding about `path`, as the detector would name it.
    fn finding_about(path: &Path) -> Finding {
        registry::row(ids::CREDENTIAL_FILE_MODE)
            .unwrap()
            .finding(Observation {
                subject: Subject::machine().path(path),
                title: "t".to_owned(),
                confidence: Confidence::High,
                addon: None,
                folder: None,
                evidence: Evidence {
                    path: Some(path.to_path_buf()),
                    ..Evidence::default()
                },
            })
    }

    #[test]
    fn plan_cases() {
        let (dir, paths) = home();
        let inside = paths.credential_file("github");
        write(&inside, b"secret", 0o644);
        let outside = dir.path().canonicalize().unwrap().join("elsewhere/key");
        write(&outside, b"secret", 0o644);
        let chmod = |path: &Path| {
            vec![Fix::Chmod {
                path: path.to_path_buf(),
                mode: Mode::new(0o600).unwrap(),
            }]
        };
        let cases: Vec<(&str, Vec<CredentialStat>, &Path, Vec<Fix>)> = vec![
            (
                "exposed_and_fixable",
                vec![stat(inside.clone(), 0o644, me(), false)],
                &inside,
                chmod(&inside),
            ),
            (
                "a_symlink",
                vec![stat(inside.clone(), 0o644, me(), true)],
                &inside,
                vec![],
            ),
            (
                "outside_config",
                vec![stat(outside.clone(), 0o644, me(), false)],
                &outside,
                vec![],
            ),
            (
                "another_users",
                vec![stat(inside.clone(), 0o644, me().wrapping_add(1), false)],
                &inside,
                vec![],
            ),
            (
                "fixed_since_the_finding",
                vec![stat(inside.clone(), 0o600, me(), false)],
                &inside,
                vec![],
            ),
            (
                "gone",
                vec![CredentialStat {
                    path: inside.clone(),
                    origin: CredentialOrigin::Stored,
                    stat: FileStat::Missing,
                }],
                &inside,
                vec![],
            ),
            ("not_listed", vec![], &inside, vec![]),
        ];
        for (name, files, path, expected) in cases {
            let view = view_of(&paths, files.clone());
            let planned = plan(&view, &finding_about(path));
            assert_eq!(planned, Ok(expected), "{name}: {files:?}");
        }
    }

    /// The detector's finding plans exactly its chmod: they agree on the path.
    #[test]
    fn the_detectors_finding_plans_its_chmod() {
        let (_dir, paths) = home();
        let file = paths.credential_file("github");
        write(&file, b"secret", 0o604);
        let view = view_of(&paths, vec![stat(file.clone(), 0o604, me(), false)]);
        let detected = credential_file_mode(&view, &Selection::all(), &mut Budget::unlimited());
        let row = registry::row(ids::CREDENTIAL_FILE_MODE).unwrap();
        let finding = row.finding(detected.found[0].clone());
        assert_eq!(
            plan(&view, &finding),
            Ok(vec![Fix::Chmod {
                path: file,
                mode: Mode::new(0o600).unwrap()
            }])
        );
    }

    #[test]
    fn spec_passes_the_invariants() {
        let row = registry::row(ids::CREDENTIAL_FILE_MODE).unwrap();
        let spec = row.fix.unwrap();
        assert!(spec.enabled && !spec.needs_force);
        assert_eq!(spec.writes_to, &[ScopeName::Config]);
        assert_eq!(spec.ops, &[FixKind::Chmod]);
        assert_eq!(spec.cardinality, Cardinality::Low);
        assert_eq!(spec.preconditions, &[FixerPrecondition::LockHeld]);
        assert!(invariant_violations(std::slice::from_ref(row)).is_empty());
    }
}
