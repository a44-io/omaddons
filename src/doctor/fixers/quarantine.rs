//! Quarantining fixers: what a failure mode's evidence names, renamed into its own
//! filesystem's quarantine root, never deleted (M4-D35, M4-D36).

use crate::doctor::detectors::caches::blob_bytes;
use crate::doctor::finding::Finding;
use crate::doctor::registry::Refusal;
use crate::doctor::view::DoctorView;
use crate::engine::repair_action::{Fix, QuarantineRoot};

/// Quarantines a damaged cached archive into this run's cache quarantine
/// (`<cache>/quarantine/<run id>/blobs/<sha256>`), whatever pins it (M4-D52: its next
/// reinstall downloads it again). The path is rebuilt from the evidence's blob name, so
/// the fix names only a file directly under `<cache>/blobs/`; `blobs/index.json` is never
/// touched (eviction lists the directory, and the next use rewrites the index). A
/// finding with no blob name in its evidence plans nothing.
pub fn blob(view: &DoctorView, finding: &Finding) -> Result<Vec<Fix>, Refusal> {
    let name = finding
        .evidence
        .expected
        .as_deref()
        .and_then(|expected| expected.strip_prefix("sha256:"))
        .filter(|name| blob_bytes(name).is_some());
    Ok(name
        .map(|name| Fix::Quarantine {
            path: view.paths().blob(name),
            root: QuarantineRoot::Cache,
        })
        .into_iter()
        .collect())
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use super::*;
    use crate::doctor::finding::Evidence;
    use crate::doctor::registry::{self, ids};
    use crate::doctor::view::{DoctorView, ViewOptions};
    use crate::engine::repair::journal::ResourcesWire;
    use crate::engine::repair::{LowerCtx, PlannedFix, lower};
    use crate::engine::repair_action::{Endpoint, Endpoints, Primitive};
    use crate::engine::testing::{REPAIR_RUN_ID, repair_home};
    use crate::model::TxId;

    const NAME: &str = "ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12";

    fn finding(expected: Option<&str>) -> Finding {
        let row = registry::row(ids::BLOB_HASH_MISMATCH).unwrap();
        let bytes = blob_bytes(NAME).unwrap();
        row.finding(crate::doctor::finding::Observation {
            subject: crate::doctor::finding::Subject::machine().blob(bytes),
            title: "Cached archive ab12ab12ab12… is damaged".to_owned(),
            confidence: crate::doctor::finding::Confidence::High,
            addon: None,
            folder: None,
            evidence: Evidence {
                expected: expected.map(str::to_owned),
                ..Evidence::default()
            },
        })
    }

    #[test]
    fn the_blob_fixer_quarantines_into_the_cache_root() {
        let home = repair_home();
        let damaged = home.paths.blob(NAME);
        std::fs::create_dir_all(damaged.parent().unwrap()).unwrap();
        std::fs::write(&damaged, b"damaged").unwrap();
        std::fs::write(home.paths.blobs_dir().join("index.json"), b"{}").unwrap();
        let view = DoctorView::load_at(home.paths.clone(), ViewOptions::default());
        let fixes = blob(&view, &finding(Some(&format!("sha256:{NAME}")))).unwrap();
        assert_eq!(
            fixes,
            vec![Fix::Quarantine {
                path: damaged.clone(),
                root: QuarantineRoot::Cache,
            }]
        );
        let planned: Vec<PlannedFix> = fixes
            .into_iter()
            .map(|fix| PlannedFix {
                fm: Cow::Borrowed(ids::BLOB_HASH_MISMATCH),
                fix,
            })
            .collect();
        let ctx = LowerCtx {
            paths: &home.paths,
            tx: TxId::new(),
            run: &home.run,
            clients: &home.planned,
            skill_dirs: &home.skill_dirs,
            scopes: &home.scopes,
            mount_id: &crate::fsx::mount_id,
        };
        let lowered = lower(&ctx, &planned).unwrap();
        let to = home
            .paths
            .cache_quarantine(REPAIR_RUN_ID)
            .join(format!("blobs/{NAME}"));
        assert_eq!(lowered.steps.len(), 1, "one primitive");
        let step = &lowered.steps[0].step;
        assert!(
            matches!(&step.op, Primitive::RenameNoReplace { from, to: dest, activation: false }
                if *from == damaged && *dest == to),
            "a rename_noreplace into the cache quarantine: {:?}",
            step.op
        );
        assert!(
            matches!(
                &step.pre,
                Endpoints::Pair {
                    path: Endpoint::File {
                        identity: Some(_),
                        ..
                    },
                    to: Endpoint::Absent,
                }
            ),
            "pre: the file by identity, nothing at its destination: {:?}",
            step.pre
        );
        let wire = ResourcesWire::of(&lowered.resources).unwrap();
        for end in [&damaged, &to] {
            let end = end.display().to_string();
            assert!(
                wire.writes.iter().any(|w| w.contains(&end)),
                "{end} in {:?}",
                wire.writes
            );
        }
        let all = serde_json::to_string(&lowered.steps.iter().map(|s| &s.step).collect::<Vec<_>>())
            .unwrap();
        assert!(
            !all.contains("index.json"),
            "no step names index.json: {all}"
        );
    }

    #[test]
    fn a_finding_without_a_blob_name_plans_nothing() {
        let home = repair_home();
        let view = DoctorView::load_at(home.paths.clone(), ViewOptions::default());
        for expected in [
            None,
            Some("sha256:index.json"),
            Some(NAME),
            Some("sha256:AB12"),
        ] {
            let fixes = blob(&view, &finding(expected)).unwrap();
            assert_eq!(fixes, Vec::new(), "{expected:?}");
        }
    }
}
