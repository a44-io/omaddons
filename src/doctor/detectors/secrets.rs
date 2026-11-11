//! `secrets`: credential files other users can read (M4-D51), judged from the Credentials
//! layer's stats alone. Nothing here opens, reads or hashes a credential file (M4-D42):
//! evidence holds a path and a mode, never a size or a byte.

use std::path::PathBuf;

use crate::doctor::budget::Budget;
use crate::doctor::finding::{
    Confidence, Detected, Evidence, Incomplete, IncompleteLimit, Observation, SecretOrigin,
    Subject, normalize,
};
use crate::doctor::selection::Selection;
use crate::doctor::view::{CredentialOrigin, CredentialStat, DoctorView, FileStat};
use crate::limits::DOCTOR_SCAN_MAX_ENTRIES;
use crate::sources::SourceId;
use crate::state::config::credential_exposed;
use crate::state::paths::Paths;

/// The mode a repair sets, as `evidence.expected` says it.
pub(crate) const EXPECTED_MODE: &str = "0600";

/// Why the doctor leaves an exposed credential to the user, as `evidence.reason` says it:
/// the chmod would leave its write scope or fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Unfixable {
    /// The path is a symlink: fsx never writes through one's final component, and the
    /// file it points at lies outside omaddons' scope.
    Symlink,
    /// The file is not inside omaddons' config directory (canonically), the only place a
    /// repair may write it.
    OutsideConfig,
    /// Another user owns it: a chmod by this one fails with `EPERM`.
    OtherOwner,
}

impl Unfixable {
    #[cfg(feature = "schemas")]
    pub(crate) const ALL: [Self; 3] = [Self::OtherOwner, Self::OutsideConfig, Self::Symlink];

    pub(crate) const fn reason(self) -> &'static str {
        match self {
            Self::Symlink => "symlink",
            Self::OutsideConfig => "outside_config",
            Self::OtherOwner => "other_owner",
        }
    }
}

/// Every `evidence.reason` this check gives, as the evidence's schema lists them.
#[cfg(feature = "schemas")]
pub(crate) const REASONS: [&str; 3] = [
    Unfixable::ALL[0].reason(),
    Unfixable::ALL[1].reason(),
    Unfixable::ALL[2].reason(),
];

/// `stat`'s path, absolute (a relative configured path against the working directory, as
/// the credential reader resolves it) and lexically normalized: its instance key's path.
pub(crate) fn credential_path(stat: &CredentialStat) -> PathBuf {
    let absolute = std::path::absolute(&stat.path).unwrap_or_else(|_| stat.path.clone());
    if absolute.is_absolute() {
        normalize(&absolute)
    } else {
        absolute
    }
}

/// Whether a repair can set `stat`'s file to 0600, checked in this order: a symlink, a
/// file outside `<config>` (canonically, as fsx matches paths: a `<config>` that is a
/// symlinked directory still counts as inside; a path that cannot be canonicalized counts
/// as outside), a file another user owns. The detector and the fixer both ask it.
pub(crate) fn fixability(paths: &Paths, stat: &CredentialStat) -> Result<(), Unfixable> {
    let FileStat::Regular { uid, symlink, .. } = stat.stat else {
        return Err(Unfixable::OutsideConfig);
    };
    if symlink {
        return Err(Unfixable::Symlink);
    }
    let path = credential_path(stat);
    let inside = match (
        path.parent().map(std::fs::canonicalize),
        std::fs::canonicalize(paths.config_dir()),
    ) {
        (Some(Ok(parent)), Ok(config)) => parent.starts_with(config),
        _ => false,
    };
    if !inside {
        return Err(Unfixable::OutsideConfig);
    }
    if uid != rustix::process::geteuid().as_raw() {
        return Err(Unfixable::OtherOwner);
    }
    Ok(())
}

/// The mode of an exposed, non-empty regular credential file; `None` for anything this
/// check does not judge: no file, not a regular file, no stat, an empty file (no secret:
/// `auth github clear` leaves one), or a mode that keeps it private.
pub(crate) fn exposed_mode(stat: &CredentialStat) -> Option<u32> {
    match stat.stat {
        FileStat::Regular { mode, bytes, .. } if bytes > 0 && credential_exposed(mode) => {
            Some(mode)
        }
        FileStat::Regular { .. }
        | FileStat::Missing
        | FileStat::NotRegular { .. }
        | FileStat::Unreadable { .. } => None,
    }
}

/// `credential-file-mode` (tier `cached`): every exposed, non-empty regular credential
/// file the Credentials layer stat'ed, stored or configured, one observation per
/// normalized path (the stored entry first when config.toml names a stored file too). An
/// instance the doctor cannot repair carries its reason and `fixable: false`. A listing
/// of `<config>/credentials/` cut at its bound is `incomplete`, never a pass (M4-D43).
pub(crate) fn credential_file_mode(view: &DoctorView, _: &Selection, _: &mut Budget) -> Detected {
    let Some(credentials) = view.credentials() else {
        return Detected::default();
    };
    let mut seen: Vec<PathBuf> = Vec::new();
    let mut found = Vec::new();
    for stat in &credentials.files {
        let Some(mode) = exposed_mode(stat) else {
            continue;
        };
        let path = credential_path(stat);
        if seen.contains(&path) {
            continue;
        }
        seen.push(path.clone());
        let manual = fixability(view.paths(), stat).err();
        found.push(observation(stat, path, mode, manual));
    }
    let incomplete = credentials.truncated.then_some(Incomplete {
        limit: IncompleteLimit::Entries,
        deadline_ms: None,
        max_entries: Some(DOCTOR_SCAN_MAX_ENTRIES),
    });
    Detected { found, incomplete }
}

/// One exposed credential's observation.
fn observation(
    stat: &CredentialStat,
    path: PathBuf,
    mode: u32,
    manual: Option<Unfixable>,
) -> Observation {
    let name = stat
        .path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let (origin, source) = match &stat.origin {
        CredentialOrigin::Stored => (SecretOrigin::File, SourceId::from_wire(&name)),
        CredentialOrigin::Configured { source } => {
            (SecretOrigin::Config, SourceId::from_wire(source))
        }
    };
    // "open to" when neither group nor others may read it, only write or run it.
    let exposure = if mode & 0o044 == 0 {
        "open to"
    } else {
        "readable by"
    };
    let title = match &stat.origin {
        // ubs:ignore — a source id, not a secret
        CredentialOrigin::Stored if source == Some(SourceId::Github) => {
            format!("The GitHub token file is {exposure} other users")
        }
        CredentialOrigin::Stored => format!("The stored {name} key file is {exposure} other users"),
        CredentialOrigin::Configured { source } => {
            format!("The credential file config.toml names for {source} is {exposure} other users")
        }
    };
    Observation {
        subject: Subject::machine().path(&path),
        title,
        confidence: Confidence::High,
        addon: None,
        folder: None,
        evidence: Evidence {
            path: Some(path),
            mode: Some(mode),
            expected: Some(EXPECTED_MODE.to_owned()),
            origin: Some(origin),
            source,
            reason: manual.map(Unfixable::reason),
            fixable: Some(manual.is_none()),
            ..Evidence::default()
        },
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::path::Path;

    use super::*;
    use crate::doctor::finding::Finding;
    use crate::doctor::registry::{self, ids};
    use crate::doctor::view::tests::home;
    use crate::doctor::view::{Credentials, LayerState, Preloaded};

    /// A regular file at `path` holding `bytes`, at `mode`.
    pub(crate) fn write(path: &Path, bytes: &[u8], mode: u32) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    fn me() -> u32 {
        rustix::process::geteuid().as_raw()
    }

    fn regular(mode: u32, bytes: u64) -> FileStat {
        FileStat::Regular {
            mode,
            uid: me(),
            bytes,
            symlink: false,
        }
    }

    fn stored(path: PathBuf, stat: FileStat) -> CredentialStat {
        CredentialStat {
            path,
            origin: CredentialOrigin::Stored,
            stat,
        }
    }

    fn configured(source: &str, path: PathBuf, stat: FileStat) -> CredentialStat {
        CredentialStat {
            path,
            origin: CredentialOrigin::Configured {
                source: source.to_owned(),
            },
            stat,
        }
    }

    /// The view of `paths` with the Credentials layer `files`.
    pub(crate) fn view_of(paths: &Paths, files: Vec<CredentialStat>) -> DoctorView {
        DoctorView::preloaded(
            paths.clone(),
            Preloaded {
                credentials: Some(LayerState::Available(Credentials {
                    files,
                    truncated: false,
                })),
                ..Preloaded::default()
            },
        )
    }

    fn detect(view: &DoctorView) -> Detected {
        credential_file_mode(view, &Selection::all(), &mut Budget::unlimited())
    }

    fn findings(detected: &Detected) -> Vec<Finding> {
        let row = registry::row(ids::CREDENTIAL_FILE_MODE).unwrap();
        detected
            .found
            .iter()
            .cloned()
            .map(|obs| row.finding(obs))
            .collect()
    }

    #[test]
    fn exposure_cases() {
        let (_dir, paths) = home();
        let file = paths.credential_file("github");
        for (mode, expected) in [
            (0o600, None),
            (
                0o644,
                Some("The GitHub token file is readable by other users"),
            ),
            (0o602, Some("The GitHub token file is open to other users")),
            (0o400, None),
        ] {
            write(&file, b"ghp_token", mode);
            let view = view_of(&paths, vec![stored(file.clone(), regular(mode, 9))]);
            let detected = detect(&view);
            let titles: Vec<&str> = detected.found.iter().map(|o| o.title.as_str()).collect();
            assert_eq!(
                titles,
                expected.into_iter().collect::<Vec<_>>(),
                "{mode:04o}"
            );
        }
    }

    #[test]
    fn credential_entry_cases() {
        let (_dir, paths) = home();
        let creds = paths.credentials_dir();
        let inside = paths.config_dir().join("keys/gh");
        let outside = paths.home().join("elsewhere/key");
        for path in [
            creds.join("github"),
            creds.join("curseforge"),
            creds.join("notes"),
            inside.clone(),
            outside.clone(),
        ] {
            write(&path, b"secret", 0o644);
        }
        let exposed = || regular(0o644, 6);
        struct Case {
            name: &'static str,
            entries: Vec<CredentialStat>,
            /// Each observation's title, origin, source and reason.
            expected: Vec<(
                &'static str,
                SecretOrigin,
                Option<SourceId>,
                Option<&'static str>,
            )>,
        }
        let cases = vec![
            Case {
                name: "missing_not_regular_unreadable_empty",
                entries: vec![
                    stored(creds.join("a"), FileStat::Missing),
                    stored(creds.join("b"), FileStat::NotRegular { symlink: false }),
                    stored(
                        creds.join("c"),
                        FileStat::Unreadable {
                            kind: std::io::ErrorKind::PermissionDenied,
                        },
                    ),
                    stored(creds.join("github"), regular(0o644, 0)),
                ],
                expected: vec![],
            },
            Case {
                name: "the_stored_github_token",
                entries: vec![stored(creds.join("github"), exposed())],
                expected: vec![(
                    "The GitHub token file is readable by other users",
                    SecretOrigin::File,
                    Some(SourceId::Github),
                    None,
                )],
            },
            Case {
                name: "another_stored_key",
                entries: vec![stored(creds.join("curseforge"), exposed())],
                expected: vec![(
                    "The stored curseforge key file is readable by other users",
                    SecretOrigin::File,
                    Some(SourceId::Curseforge),
                    None,
                )],
            },
            Case {
                name: "a_stored_file_of_no_source",
                entries: vec![stored(creds.join("notes"), exposed())],
                expected: vec![(
                    "The stored notes key file is readable by other users",
                    SecretOrigin::File,
                    None,
                    None,
                )],
            },
            Case {
                name: "a_configured_file_inside_config",
                entries: vec![configured("github", inside.clone(), exposed())],
                expected: vec![(
                    "The credential file config.toml names for github is readable by other users",
                    SecretOrigin::Config,
                    Some(SourceId::Github),
                    None,
                )],
            },
            Case {
                name: "a_configured_file_outside_config",
                entries: vec![configured("github", outside.clone(), exposed())],
                expected: vec![(
                    "The credential file config.toml names for github is readable by other users",
                    SecretOrigin::Config,
                    Some(SourceId::Github),
                    Some("outside_config"),
                )],
            },
            Case {
                name: "the_stored_token_also_configured_is_one_finding_stored_first",
                entries: vec![
                    stored(creds.join("github"), exposed()),
                    configured("github", creds.join("./github"), exposed()),
                ],
                expected: vec![(
                    "The GitHub token file is readable by other users",
                    SecretOrigin::File,
                    Some(SourceId::Github),
                    None,
                )],
            },
        ];
        for case in cases {
            let view = view_of(&paths, case.entries.clone());
            let detected = detect(&view);
            let got: Vec<_> = detected
                .found
                .iter()
                .map(|o| {
                    (
                        o.title.as_str(),
                        o.evidence.origin.unwrap(),
                        o.evidence.source,
                        o.evidence.reason,
                    )
                })
                .collect();
            assert_eq!(got, case.expected, "{}: {:?}", case.name, case.entries);
            for obs in &detected.found {
                assert_eq!(
                    obs.evidence.fixable,
                    Some(obs.evidence.reason.is_none()),
                    "{}",
                    case.name
                );
            }
        }
    }

    #[test]
    fn a_relative_configured_path_is_absolute_in_the_instance_key() {
        let (_dir, paths) = home();
        let stat = configured("github", PathBuf::from("rel/key"), regular(0o644, 6));
        let view = view_of(&paths, vec![stat]);
        let found = findings(&detect(&view));
        assert_eq!(found.len(), 1);
        let cwd = std::env::current_dir().unwrap();
        let expected = cwd.join("rel/key");
        assert_eq!(
            found[0].instance.as_str(),
            format!(
                "fm-secrets-credential-file-mode@machine:path={}",
                expected.display()
            )
        );
        assert_eq!(found[0].evidence.path.as_deref(), Some(expected.as_path()));
    }

    #[test]
    fn fixability_order() {
        let (dir, paths) = home();
        let root = dir.path().canonicalize().unwrap();
        let real = paths.credentials_dir().join("real");
        write(&real, b"secret", 0o644);
        let link_inside = paths.credentials_dir().join("github");
        symlink(&real, &link_inside).unwrap();
        let outside = root.join("elsewhere/key");
        write(&outside, b"secret", 0o644);
        let link_outside = root.join("elsewhere/link");
        symlink(&outside, &link_outside).unwrap();
        // A <config> reached through a symlinked directory: still inside.
        let dotfiles = root.join("dotfiles");
        write(
            &dotfiles.join("omaddons/credentials/github"),
            b"secret",
            0o644,
        );
        symlink(&dotfiles, root.join("cfglink")).unwrap();
        let linked = Paths::with_roots(
            &root,
            root.join("cfglink"),
            root.join("state"),
            root.join("cache"),
        );
        let link_stat = || FileStat::Regular {
            mode: 0o644,
            uid: me(),
            bytes: 6,
            symlink: true,
        };
        let cases: Vec<(&str, &Paths, CredentialStat, Result<(), Unfixable>)> = vec![
            (
                "a_symlink_inside_config",
                &paths,
                stored(link_inside.clone(), link_stat()),
                Err(Unfixable::Symlink),
            ),
            (
                "a_regular_file_outside",
                &paths,
                configured("github", outside.clone(), regular(0o644, 6)),
                Err(Unfixable::OutsideConfig),
            ),
            (
                "config_through_a_symlinked_directory",
                &linked,
                stored(linked.credential_file("github"), regular(0o644, 6)),
                Ok(()),
            ),
            (
                "another_users_file",
                &paths,
                stored(
                    real.clone(),
                    FileStat::Regular {
                        mode: 0o644,
                        uid: me().wrapping_add(1),
                        bytes: 6,
                        symlink: false,
                    },
                ),
                Err(Unfixable::OtherOwner),
            ),
            (
                "a_symlink_outside_the_first_rule_wins",
                &paths,
                configured("github", link_outside.clone(), link_stat()),
                Err(Unfixable::Symlink),
            ),
            (
                "a_regular_file_of_ours_inside",
                &paths,
                stored(real.clone(), regular(0o644, 6)),
                Ok(()),
            ),
        ];
        for (name, paths, stat, expected) in cases {
            assert_eq!(fixability(paths, &stat), expected, "{name}: {stat:?}");
        }
    }

    #[test]
    fn evidence_never_holds_content() {
        let (_dir, paths) = home();
        let file = paths.credential_file("github");
        write(&file, b"ghp_SECRET_TOKEN_TEXT", 0o640);
        let view = view_of(&paths, vec![stored(file.clone(), regular(0o640, 21))]);
        let found = findings(&detect(&view));
        let evidence = serde_json::to_value(&found[0].evidence).unwrap();
        let mut keys: Vec<&str> = evidence
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            ["expected", "mode", "origin", "path", "source"],
            "{evidence}"
        );
        assert_eq!(evidence["mode"], "0640");
        assert_eq!(evidence["expected"], "0600");
        let text = serde_json::to_string(&found[0]).unwrap();
        assert!(!text.contains("SECRET"), "{text}");
        assert!(!text.contains("21"), "no size: {text}");
    }

    #[test]
    fn instance_keys_by_path() {
        let (_dir, paths) = home();
        let a = paths.credential_file("github");
        let b = paths.credential_file("curseforge");
        write(&a, b"x", 0o644);
        write(&b, b"y", 0o644);
        let view = view_of(
            &paths,
            vec![
                stored(a.clone(), regular(0o644, 1)),
                stored(b.clone(), regular(0o644, 1)),
            ],
        );
        let found = findings(&detect(&view));
        let keys: Vec<(String, String)> = found
            .iter()
            .map(|f| (f.instance.as_str().to_owned(), f.label.clone()))
            .collect();
        assert_eq!(
            keys,
            [a, b]
                .iter()
                .map(|p| (
                    format!(
                        "fm-secrets-credential-file-mode@machine:path={}",
                        p.display()
                    ),
                    format!("machine · {}", p.display())
                ))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn remediation_by_fixability() {
        let (dir, paths) = home();
        let inside = paths.credential_file("github");
        write(&inside, b"x", 0o644);
        let outside = dir.path().canonicalize().unwrap().join("elsewhere/key");
        write(&outside, b"x", 0o644);
        let view = view_of(
            &paths,
            vec![
                stored(inside.clone(), regular(0o644, 1)),
                configured("github", outside.clone(), regular(0o644, 1)),
            ],
        );
        let found = findings(&detect(&view));
        let (fixable, manual) = (&found[0].remediation, &found[1].remediation);
        assert!(fixable.auto_fixable && !fixable.needs_force && !fixable.destructive);
        assert_eq!(
            fixable.argv.as_deref(),
            Some(
                &[
                    "omaddons",
                    "doctor",
                    "fix",
                    "--only",
                    "fm-secrets-credential-file-mode"
                ]
                .map(str::to_owned)[..]
            )
        );
        assert!(!manual.auto_fixable);
        assert_eq!(
            (manual.command.clone(), manual.argv.clone()),
            (format!("chmod 600 {}", outside.display()), None)
        );
    }

    #[test]
    fn a_truncated_listing_is_incomplete() {
        let (_dir, paths) = home();
        let view = DoctorView::preloaded(
            paths,
            Preloaded {
                credentials: Some(LayerState::Available(Credentials {
                    files: Vec::new(),
                    truncated: true,
                })),
                ..Preloaded::default()
            },
        );
        assert_eq!(
            detect(&view).incomplete.map(|i| i.limit),
            Some(IncompleteLimit::Entries)
        );
    }

    /// The detector and the chmod fixer never open a credential file: their production
    /// code calls no reader.
    #[test]
    fn secrets_and_chmod_never_read_a_credential() {
        use crate::source_scan::{Literals, production_code};
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        for file in [
            "src/doctor/detectors/secrets.rs",
            "src/doctor/fixers/chmod.rs",
        ] {
            let text = std::fs::read_to_string(root.join(file)).unwrap();
            let code = production_code(&text, Literals::Blank).unwrap();
            for forbidden in [
                "read(",
                "read_to_string",
                "read_to_end",
                "File::open",
                "OpenOptions",
                "fs::read",
            ] {
                assert!(!code.contains(forbidden), "{file} calls {forbidden}");
            }
        }
    }
}
