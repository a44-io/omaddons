//! `caches`: omaddons' own derived caches.

use crate::doctor::budget::Budget;
use crate::doctor::finding::{
    Confidence, Detected, Evidence, Incomplete, IncompleteLimit, Observation, Subject,
};
use crate::doctor::selection::Selection;
use crate::doctor::view::{DoctorView, EntryKind};
use crate::engine::blobs::{self, Verification};
use crate::limits::DOCTOR_SCAN_MAX_ENTRIES;

/// A blob name's 32 bytes; `None` for a name that is no blob's (64 lowercase hex).
pub(crate) fn blob_bytes(name: &str) -> Option<[u8; 32]> {
    if !blobs::is_sha256(name) {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(name.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

/// `blob-hash-mismatch` (tier `deep`, M4-D52): every cached archive re-hashed against its
/// name, through [`blobs::verify_blob`] alone (never `BlobCache`, whose reads write the
/// index), one entry charged per blob and the deadline checked after each MiB. One
/// observation per blob whose bytes differ or that cannot be read, in name order; a
/// listing cut at its bound, or a budget spent, is `incomplete`, never a pass (M4-D43).
/// `index.json`, temp files, directories, symlinks and other names are never reported,
/// as eviction ignores them.
pub(crate) fn blob_hash_mismatch(
    view: &DoctorView,
    _: &Selection,
    budget: &mut Budget,
) -> Detected {
    let Some(caches) = view.caches() else {
        return Detected::default();
    };
    let listing = &caches.blobs;
    let mut found = Vec::new();
    for entry in &listing.entries {
        let Some(bytes) = blob_bytes(&entry.name).filter(|_| entry.kind == EntryKind::File) else {
            continue;
        };
        if budget.charge(1).is_err() {
            return Detected {
                found,
                incomplete: budget.incomplete(),
            };
        }
        let path = listing.dir.join(&entry.name);
        let name = entry.name.as_str();
        let short = &name[..12];
        let expected = Some(format!("sha256:{name}"));
        let observation = |title: String, evidence: Evidence| Observation {
            subject: Subject::machine().blob(bytes),
            title,
            confidence: Confidence::High,
            addon: None,
            folder: None,
            evidence,
        };
        match blobs::verify_blob(&path, name, &mut || budget.check().is_err()) {
            Verification::Matches { .. } | Verification::Gone => {}
            Verification::Differs { actual, .. } => found.push(observation(
                format!("Cached archive {short}… is damaged"),
                Evidence {
                    path: Some(path.clone()),
                    expected,
                    observed: Some(format!("sha256:{actual}")),
                    ..Evidence::default()
                },
            )),
            Verification::Unreadable { .. } => found.push(observation(
                format!("Cached archive {short}… cannot be read"),
                Evidence {
                    path: Some(path.clone()),
                    expected,
                    reason: Some("unreadable"),
                    ..Evidence::default()
                },
            )),
            Verification::Stopped => {
                return Detected {
                    found,
                    incomplete: budget.incomplete(),
                };
            }
        }
    }
    let incomplete = listing.truncated.then_some(Incomplete {
        limit: IncompleteLimit::Entries,
        deadline_ms: None,
        max_entries: Some(DOCTOR_SCAN_MAX_ENTRIES),
    });
    Detected { found, incomplete }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use sha2::{Digest, Sha256};

    use super::*;
    use crate::doctor::finding::Finding;
    use crate::doctor::registry::{self, ids};
    use crate::doctor::view::tests::{home, is_root, view_at};
    use crate::doctor::view::{Caches, LayerState, ListedEntry, Listing, Preloaded};
    use crate::state::paths::Paths;

    fn sha(bytes: &[u8]) -> String {
        Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    /// A blob named by `name`'s digest holding `bytes`; its name.
    fn blob(paths: &Paths, of: &[u8], bytes: &[u8]) -> String {
        let name = sha(of);
        let dir = paths.blobs_dir();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(&name), bytes).unwrap();
        name
    }

    fn run(view: &DoctorView, budget: &mut Budget) -> Detected {
        blob_hash_mismatch(view, &Selection::all(), budget)
    }

    fn findings(view: &DoctorView) -> (Vec<Finding>, Option<Incomplete>) {
        let row = registry::row(ids::BLOB_HASH_MISMATCH).unwrap();
        let detected = run(view, &mut Budget::unlimited());
        let found = detected.found.into_iter().map(|o| row.finding(o)).collect();
        (found, detected.incomplete)
    }

    /// Each finding's (title, path, expected, observed, reason).
    type Seen = (
        String,
        Option<PathBuf>,
        Option<String>,
        Option<String>,
        Option<&'static str>,
    );

    fn seen(found: &[Finding]) -> Vec<Seen> {
        found
            .iter()
            .map(|f| {
                let e = &f.evidence;
                (
                    f.title.clone(),
                    e.path.clone(),
                    e.expected.clone(),
                    e.observed.clone(),
                    e.reason,
                )
            })
            .collect()
    }

    fn damaged(paths: &Paths, name: &str, title: &str, actual: &[u8]) -> Seen {
        (
            format!("Cached archive {}… {title}", &name[..12]),
            Some(paths.blob(name)),
            Some(format!("sha256:{name}")),
            Some(format!("sha256:{}", sha(actual))),
            None,
        )
    }

    #[test]
    fn blob_detector_cases() {
        for case in [
            "clean",
            "one_damaged",
            "two_damaged_in_name_order",
            "ignored_entries",
            "no_blobs_dir",
        ] {
            let (_dir, paths) = home();
            let mut want: Vec<Seen> = Vec::new();
            match case {
                "clean" => {
                    for text in ["a", "b", "c"] {
                        blob(&paths, text.as_bytes(), text.as_bytes());
                    }
                }
                "one_damaged" => {
                    blob(&paths, b"good", b"good");
                    let name = blob(&paths, b"release", b"releasf");
                    want.push(damaged(&paths, &name, "is damaged", b"releasf"));
                }
                "two_damaged_in_name_order" => {
                    let mut damaged_two: Vec<(String, &[u8])> = [b"one".as_slice(), b"two"]
                        .iter()
                        .map(|of| (blob(&paths, of, b"broken"), b"broken".as_slice()))
                        .collect();
                    damaged_two.sort();
                    want.extend(
                        damaged_two
                            .iter()
                            .map(|(n, a)| damaged(&paths, n, "is damaged", a)),
                    );
                }
                "ignored_entries" => {
                    let dir = paths.blobs_dir();
                    std::fs::create_dir_all(&dir).unwrap();
                    std::fs::write(dir.join("index.json"), b"{}").unwrap();
                    std::fs::write(dir.join(".omaddons-tmp-01M6"), b"partial").unwrap();
                    std::fs::write(dir.join(sha(b"x").to_uppercase()), b"wrong").unwrap();
                    std::fs::create_dir(dir.join(sha(b"a directory"))).unwrap();
                    let target = dir.join("index.json");
                    symlink(&target, dir.join(sha(b"a symlink"))).unwrap();
                }
                _ => {}
            }
            let view = view_at(paths.clone());
            let (found, incomplete) = findings(&view);
            assert_eq!(seen(&found), want, "case {case}");
            assert_eq!(incomplete, None, "case {case}: complete");
            if case == "no_blobs_dir" {
                assert!(
                    view.caches().is_some(),
                    "case {case}: the layer is available"
                );
            }
        }
    }

    #[test]
    fn an_unreadable_blob_is_a_finding() {
        if is_root() {
            // Root reads a mode-000 file: the case cannot be made.
            return;
        }
        let (_dir, paths) = home();
        let name = blob(&paths, b"release", b"release");
        let path = paths.blob(&name);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        let (found, _) = findings(&view_at(paths.clone()));
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let want = vec![(
            format!("Cached archive {}… cannot be read", &name[..12]),
            Some(path),
            Some(format!("sha256:{name}")),
            None,
            Some("unreadable"),
        )];
        assert_eq!(seen(&found), want, "case unreadable (mode 000)");
    }

    #[test]
    fn the_blob_detector_stops_at_its_budget() {
        let (_dir, paths) = home();
        for of in [b"one".as_slice(), b"two"] {
            blob(&paths, of, b"broken");
        }
        let view = view_at(paths);
        let detected = run(&view, &mut Budget::new(Duration::from_secs(60), 1));
        assert_eq!(detected.found.len(), 1, "one entry: one blob hashed");
        assert_eq!(
            detected.incomplete.map(|i| i.limit),
            Some(IncompleteLimit::Entries)
        );
        let detected = run(&view, &mut Budget::new(Duration::ZERO, 100));
        assert_eq!(
            detected.found.len(),
            0,
            "an expired deadline: nothing hashed"
        );
        assert_eq!(
            detected.incomplete.map(|i| i.limit),
            Some(IncompleteLimit::Deadline)
        );
    }

    #[test]
    fn a_truncated_listing_is_incomplete() {
        let (_dir, paths) = home();
        let name = blob(&paths, b"good", b"good");
        let listing = Listing {
            dir: paths.blobs_dir(),
            entries: vec![ListedEntry {
                name,
                kind: EntryKind::File,
                bytes: 4,
            }],
            truncated: true,
        };
        let view = DoctorView::preloaded(
            paths,
            Preloaded {
                caches: Some(LayerState::Available(Caches { blobs: listing })),
                ..Preloaded::default()
            },
        );
        let detected = run(&view, &mut Budget::unlimited());
        assert_eq!(detected.found, Vec::new(), "the listed blob matches");
        assert_eq!(
            detected.incomplete.map(|i| i.limit),
            Some(IncompleteLimit::Entries)
        );
    }

    /// Every path under `root` with its bytes, for an unchanged-tree check.
    fn tree(root: &Path) -> Vec<(PathBuf, Option<Vec<u8>>)> {
        let mut out = Vec::new();
        let mut pending = vec![root.to_path_buf()];
        while let Some(dir) = pending.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if std::fs::symlink_metadata(&path).unwrap().is_dir() {
                    pending.push(path.clone());
                    out.push((path, None));
                } else {
                    out.push((path.clone(), std::fs::read(&path).ok()));
                }
            }
        }
        out.sort();
        out
    }

    #[test]
    fn the_blob_detector_writes_nothing() {
        let (dir, paths) = home();
        blob(&paths, b"good", b"good");
        blob(&paths, b"release", b"damaged");
        std::fs::write(paths.blobs_dir().join("index.json"), b"{\"entries\":{}}").unwrap();
        let before = tree(dir.path());
        let (found, _) = findings(&view_at(paths));
        assert_eq!(found.len(), 1);
        assert_eq!(
            tree(dir.path()),
            before,
            "the home, index.json included, is unchanged"
        );
    }

    #[test]
    fn the_blob_finding_has_its_registry_shape() {
        let (_dir, paths) = home();
        let name = blob(&paths, b"release", b"damaged");
        let view = view_at(paths);
        let (found, _) = findings(&view);
        let f = &found[0];
        let row = registry::row(ids::BLOB_HASH_MISMATCH).unwrap();
        let json = serde_json::to_value(f).unwrap();
        assert_eq!(json["id"], "fm-caches-blob-hash-mismatch");
        assert_eq!(json["severity"], "P2");
        assert_eq!(json["subsystem"], "caches");
        assert_eq!(
            json["instance"],
            format!("fm-caches-blob-hash-mismatch@machine:blob={name}")
        );
        assert_eq!(json["label"], format!("machine · blob {}", &name[..12]));
        assert_eq!(json["confidence"], "high");
        for absent in ["target", "addon", "folder"] {
            assert!(json[absent].is_null(), "{absent}: {}", json[absent]);
        }
        let remediation =
            crate::doctor::fix::remediation_for(&view, f, row.fix.as_ref().unwrap(), None);
        let json = serde_json::to_value(&remediation).unwrap();
        assert_eq!(
            json["command"],
            "omaddons doctor fix --only fm-caches-blob-hash-mismatch"
        );
        assert_eq!(
            json["argv"],
            serde_json::json!([
                "omaddons",
                "doctor",
                "fix",
                "--only",
                "fm-caches-blob-hash-mismatch"
            ])
        );
        assert_eq!(
            json["explain_command"],
            "omaddons doctor explain fm-caches-blob-hash-mismatch"
        );
        assert_eq!(json["auto_fixable"], true);
        assert_eq!(json["needs_force"], false);
        assert_eq!(json["estimated_actions"], 1);
        assert_eq!(json["destructive"], false);
    }

    #[test]
    fn blob_bytes_reads_a_blob_name() {
        let name = sha(b"x");
        let bytes = blob_bytes(&name).unwrap();
        let back: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(back, name);
        for bad in ["index.json", &name.to_uppercase(), &name[..63]] {
            assert_eq!(blob_bytes(bad), None, "{bad}");
        }
    }
}
