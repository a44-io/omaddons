//! `status.json`: a derived summary plus per-target generations, rewritten after each
//! commit and undo. The widget's signal is the doorbell instead (`doorbell.rs`, M3-D4),
//! because this file misses update checks and settings changes. A writer never replaces
//! a file whose generation for any shared target is newer than its own, so a slow writer
//! cannot roll the summary back. `status --cached` derives counts from the records, so a
//! lost race only leaves the summary behind; the small check-then-rename window is
//! accepted for the same reason.

use std::collections::BTreeMap;

use jiff::Timestamp;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::paths::Paths;
use super::records::TargetRecord;
use super::store::{self, SchemaVersion, VersionStatus, Versioned};
use crate::error::{ErrorCode, Result};
use crate::fsx::{Content, Op, WriteSession, mutate};
use crate::model::{AddonState, TargetKey};

/// The summary file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StatusFile {
    #[schemars(schema_with = "crate::contract::schema::timestamp")]
    pub generated_at: Timestamp,
    pub generations: BTreeMap<TargetKey, u64>,
    pub targets: BTreeMap<TargetKey, TargetSummary>,
}

impl Versioned for StatusFile {
    const KIND: &'static str = "status";
    const CURRENT: SchemaVersion = SchemaVersion::new(1, 0);
}

/// One target's line in the summary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TargetSummary {
    pub id: String,
    pub family: String,
    pub version: String,
    pub interface: u32,
    pub managed: u32,
    pub degraded: u32,
}

/// Identity metadata the caller supplies per target (no dependency on the registry).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetMeta {
    pub id: String,
    pub family: String,
    pub version: String,
    pub interface: u32,
}

impl StatusFile {
    /// Derives the summary from the current records; targets without metadata are
    /// left out of `targets` but keep their generation.
    pub fn build(records: &[TargetRecord], meta: &BTreeMap<TargetKey, TargetMeta>) -> Self {
        let generations = records.iter().map(|r| (r.key, r.generation)).collect();
        let targets = records
            .iter()
            .filter_map(|record| {
                let meta = meta.get(&record.key)?;
                let count = |state: AddonState| {
                    let n = record.addons.values().filter(|a| a.state == state).count();
                    u32::try_from(n).unwrap_or(u32::MAX)
                };
                Some((
                    record.key,
                    TargetSummary {
                        id: meta.id.clone(),
                        family: meta.family.clone(),
                        version: meta.version.clone(),
                        interface: meta.interface,
                        managed: u32::try_from(record.addons.len()).unwrap_or(u32::MAX),
                        degraded: count(AddonState::Degraded),
                    },
                ))
            })
            .collect();
        Self {
            generated_at: Timestamp::now(),
            generations,
            targets,
        }
    }
}

/// What `write_monotonic` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteOutcome {
    Written,
    /// A newer generation (or a newer omaddons) already wrote the file.
    Skipped,
}

/// Writes `new` unless the file on disk has a higher generation for a target both
/// know. Keys present in only one of them never block, so a new or removed target
/// cannot freeze the summary. A corrupt file is simply replaced: it is derived data.
pub fn write_monotonic(
    s: &mut WriteSession<'_>,
    paths: &Paths,
    new: &StatusFile,
) -> Result<WriteOutcome> {
    let path = paths.status_file();
    match store::load::<StatusFile>(&path) {
        Ok(Some(existing)) => {
            if existing.status == VersionStatus::Newer {
                return Ok(WriteOutcome::Skipped);
            }
            let stale = new.generations.iter().any(|(key, generation)| {
                existing
                    .value
                    .generations
                    .get(key)
                    .is_some_and(|on_disk| on_disk > generation)
            });
            if stale {
                return Ok(WriteOutcome::Skipped);
            }
        }
        Ok(None) => {}
        Err(err) if err.code == ErrorCode::StateTooNew => return Ok(WriteOutcome::Skipped),
        Err(err) if err.code == ErrorCode::StateCorrupt => {}
        Err(err) => return Err(err),
    }
    let json = store::encode(new)?;
    let op = Op::WriteFile {
        content: Content::Bytes(&json),
        mode: Some(0o644),
    };
    mutate(s, &path, op)?;
    Ok(WriteOutcome::Written)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsx::{ScopeTable, Writer};

    const A: &str = "01JB8Q2V9W0000000000000000";
    const B: &str = "01JB8Q2V9X0000000000000000";

    fn status(generations: &[(&str, u64)]) -> StatusFile {
        StatusFile {
            generated_at: "2026-10-14T21:00:00Z".parse().unwrap(),
            generations: generations
                .iter()
                .map(|(k, g)| (k.parse().unwrap(), *g))
                .collect(),
            targets: BTreeMap::new(),
        }
    }

    #[test]
    fn writes_are_monotonic_per_shared_target() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let paths = Paths::with_roots(
            &root,
            root.join(".config"),
            root.join(".local/state"),
            root.join(".cache"),
        );
        std::fs::create_dir_all(paths.state_dir()).unwrap();
        let scopes = ScopeTable::new(&paths, &[], None).unwrap();
        let mut s = WriteSession::untracked(Writer::Mutating, &scopes, "status.json");
        let mut write =
            |gens: &[(&str, u64)]| write_monotonic(&mut s, &paths, &status(gens)).unwrap();
        let on_disk = |paths: &Paths| {
            store::load::<StatusFile>(&paths.status_file())
                .unwrap()
                .unwrap()
                .value
                .generations
        };

        assert_eq!(write(&[(A, 5)]), WriteOutcome::Written);
        assert_eq!(write(&[(A, 4)]), WriteOutcome::Skipped, "an older writer");
        assert_eq!(write(&[(A, 5)]), WriteOutcome::Written, "equal");
        assert_eq!(write(&[(A, 6)]), WriteOutcome::Written, "newer");
        assert_eq!(
            write(&[(B, 1)]),
            WriteOutcome::Written,
            "disjoint keys never block"
        );
        assert_eq!(
            write(&[(A, 2), (B, 9)]),
            WriteOutcome::Written,
            "A is gone from the file now"
        );
        assert_eq!(
            write(&[(A, 3), (B, 8)]),
            WriteOutcome::Skipped,
            "B regressed"
        );
        assert_eq!(on_disk(&paths).get(&B.parse().unwrap()), Some(&9));

        std::fs::write(paths.status_file(), b"{corrupt").unwrap();
        assert_eq!(
            write(&[(A, 1)]),
            WriteOutcome::Written,
            "a corrupt summary is replaced"
        );
        std::fs::write(
            paths.status_file(),
            br#"{"schema_version":"2.0","generations":{}}"#,
        )
        .unwrap();
        assert_eq!(
            write(&[(A, 9)]),
            WriteOutcome::Skipped,
            "never overwrite a newer omaddons"
        );
    }

    #[test]
    fn build_counts_managed_and_degraded_addons() {
        let key: TargetKey = A.parse().unwrap();
        let mut record = TargetRecord::empty(key);
        record.generation = 3;
        let meta = BTreeMap::from([(
            key,
            TargetMeta {
                id: "retail".into(),
                family: "mainline".into(),
                version: "12.1.5".into(),
                interface: 120_105,
            },
        )]);
        let built = StatusFile::build(
            &[record.clone(), TargetRecord::empty(B.parse().unwrap())],
            &meta,
        );
        assert_eq!(built.generations.get(&key), Some(&3));
        assert_eq!(built.generations.len(), 2);
        let summary = built.targets.get(&key).unwrap();
        assert_eq!(
            (summary.id.as_str(), summary.managed, summary.degraded),
            ("retail", 0, 0)
        );
        assert_eq!(
            built.targets.len(),
            1,
            "targets without metadata keep only a generation"
        );
    }
}
