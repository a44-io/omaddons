//! `backup verify` (M3-D47): what a backup's content is proved to be, checked or
//! established. Under the backup's reader reservation (M3-D48, so a prune finds it busy),
//! its tree is checked against its manifest, then every file is hashed and compared with
//! the manifest's hashes when it has them. A backup with none gets them recorded as a
//! first, `later` baseline, which proves only that nothing changes after it, never that
//! the capture was intact, and every surface says so.
//!
//! A real run writes the result through fsx, the manifest first and `backup.json` second:
//! `integrity` is the commit point, so a crash between the two leaves hashes the next
//! verify compares against, never a false `damaged`. It takes no global lock and appends
//! no history line (a check, not a change of settings); it rings the doorbell, since
//! `backup list` and the widget show what it recorded (M3-D4).

use jiff::Timestamp;

use super::create::write;
use super::export::reserve;
use super::tree::hash_files;
use super::{
    BackupMeta, BackupRef, Baseline, EntryKind, Integrity, Manifest, check_structure, meta_file,
};
use crate::error::{Error, ErrorCode, Result, Suggestion};
use crate::fsx::{ScopeTable, WriteSession, Writer};
use crate::model::Actor;
use crate::output::ProgressSink;
use crate::privacy::WtfPathRedactor;
use crate::state::doorbell;
use crate::state::paths::{ClientPaths, Paths};
use crate::state::store;

/// What a verify found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyOutcome {
    /// Every file matched the manifest's hashes, which keep the baseline they had.
    Verified { against: Baseline },
    /// There were no hashes: they are recorded now, as a `later` baseline (or would be,
    /// under `--dry-run`).
    BaselineRecorded,
    /// `files` files differ from their recorded hashes; `first` is the first, redacted.
    Mismatch { files: u64, first: String },
    /// The tree is not what the manifest says (`first` redacted, when known).
    Structure { first: Option<String> },
}

impl VerifyOutcome {
    /// The integrity it establishes.
    pub fn integrity(&self) -> Integrity {
        match self {
            Self::Verified { .. } | Self::BaselineRecorded => Integrity::Verified,
            Self::Mismatch { .. } | Self::Structure { .. } => Integrity::Damaged,
        }
    }

    /// The baseline a verified backup's hashes have.
    pub fn baseline(&self) -> Option<Baseline> {
        match self {
            Self::Verified { against } => Some(*against),
            Self::BaselineRecorded => Some(Baseline::Later),
            Self::Mismatch { .. } | Self::Structure { .. } => None,
        }
    }
}

/// One backup verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verified {
    pub outcome: VerifyOutcome,
    /// Its regular files, by its manifest.
    pub files: u64,
    /// Of those, hashed (none when its structure differs).
    pub checked: u64,
    /// Of those, differing from a recorded hash.
    pub mismatched: u64,
    /// The result was written (a real run).
    pub recorded: bool,
}

/// What to verify, and for whom.
pub struct VerifyRequest<'a> {
    pub backup: &'a BackupRef,
    /// Write the result (`false` under `--dry-run`).
    pub record: bool,
    /// The command and actor the doorbell rings for.
    pub command: &'a str,
    pub actor: Actor,
}

/// Verifies `req.backup` (M3-D47), reserved for the whole run; writes the result unless
/// `req.record` is false. A backup whose description or manifest does not read is
/// `state_corrupt`; one moved or deleted is `not_found`.
pub fn verify(
    paths: &Paths,
    scopes: &ScopeTable,
    req: &VerifyRequest<'_>,
    progress: &ProgressSink,
) -> Result<Verified> {
    let b = req.backup;
    let _reservation = reserve(b)?;
    let unreadable = || {
        Error::new(
            ErrorCode::StateCorrupt,
            format!(
                "backup {} cannot be verified: its description or manifest does not read",
                b.id
            ),
            Suggestion::command(["omaddons", "backup", "list"]),
        )
        .details(serde_json::json!({ "backup": b.id }))
    };
    let meta = b.meta.clone().ok_or_else(unreadable)?;
    let manifest = store::load::<Manifest>(&b.dir.join("manifest.json"))
        .ok()
        .flatten()
        .ok_or_else(unreadable)?
        .value;
    let names = WtfPathRedactor::for_client(&ClientPaths::new(&b.dir));
    let files = manifest
        .entries
        .iter()
        .filter(|e| e.kind == EntryKind::File)
        .count() as u64;
    let check = check_structure(b)?;
    let (outcome, checked, mismatched, hashed) = if check.complete() {
        let hashes =
            hash_files(&b.wtf(), &manifest.entries, progress).map_err(|e| e.error(&names))?;
        let (outcome, mismatched, hashed) = judge(&meta, &manifest, &hashes, &names);
        (outcome, files, mismatched, hashed)
    } else {
        let first = check.first.as_deref().map(|p| names.rel(p));
        (VerifyOutcome::Structure { first }, 0, 0, None)
    };
    if req.record {
        record(paths, scopes, req, meta, hashed.as_ref(), &outcome)?;
    }
    Ok(Verified {
        outcome,
        files,
        checked,
        mismatched,
        recorded: req.record,
    })
}

/// The outcome of comparing `hashes` (one per manifest entry, `None` for what is not a
/// file) with `manifest`'s, the mismatch count, and the manifest to record when hashes
/// were missing. A file without a recorded hash beside files with one (a manifest
/// written partly by hand) is judged by those it has: any difference is damage, and the
/// missing ones are recorded as a `later` baseline.
fn judge(
    meta: &BackupMeta,
    manifest: &Manifest,
    hashes: &[Option<String>],
    names: &WtfPathRedactor,
) -> (VerifyOutcome, u64, Option<Manifest>) {
    let mut mismatched = 0;
    let mut first = None;
    // A backup of no files has no hashes either: its first verify is a baseline too.
    let mut missing = meta.baseline.is_none()
        && !manifest
            .entries
            .iter()
            .any(|e| e.kind == EntryKind::File && e.sha256.is_some());
    for (entry, now) in manifest.entries.iter().zip(hashes) {
        if entry.kind != EntryKind::File {
            continue;
        }
        match (&entry.sha256, now) {
            (Some(want), Some(now)) if want.eq_ignore_ascii_case(now) => {}
            (Some(_), _) => {
                mismatched += 1;
                first.get_or_insert_with(|| entry.path.clone());
            }
            (None, _) => missing = true,
        }
    }
    if let Some(first) = first {
        let outcome = VerifyOutcome::Mismatch {
            files: mismatched,
            first: names.rel(&first),
        };
        return (outcome, mismatched, None);
    }
    if missing {
        let mut recorded = manifest.clone();
        for (entry, now) in recorded.entries.iter_mut().zip(hashes) {
            if entry.kind == EntryKind::File {
                entry.sha256.clone_from(now);
            }
        }
        return (VerifyOutcome::BaselineRecorded, 0, Some(recorded));
    }
    // Hashes with no baseline: the first verify recorded them and died before its
    // `backup.json`; they are a `later` baseline.
    let against = meta.baseline.unwrap_or(Baseline::Later);
    (VerifyOutcome::Verified { against }, 0, None)
}

/// Writes `outcome`: the manifest with the hashes recorded (when there are any), then
/// `backup.json`, whose `integrity` commits it; then rings the doorbell.
fn record(
    paths: &Paths,
    scopes: &ScopeTable,
    req: &VerifyRequest<'_>,
    mut meta: BackupMeta,
    hashed: Option<&Manifest>,
    outcome: &VerifyOutcome,
) -> Result<()> {
    let b = req.backup;
    let mut s = WriteSession::untracked(Writer::Backup, scopes, "verify");
    if let Some(manifest) = hashed {
        write(&mut s, &b.dir.join("manifest.json"), manifest)?;
    }
    meta.integrity = outcome.integrity();
    if let Some(baseline) = outcome.baseline() {
        meta.baseline = Some(baseline);
        meta.verified_at = Some(Timestamp::now());
    }
    write(&mut s, &meta_file(&b.dir), &meta)?;
    doorbell::ring(paths, req.command, req.actor);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backup::ManifestEntry;

    fn file(path: &str, sha256: Option<&str>) -> ManifestEntry {
        ManifestEntry {
            path: path.to_owned(),
            kind: EntryKind::File,
            size: 1,
            mode: 0o600,
            mtime_ns: 0,
            link: None,
            sha256: sha256.map(str::to_owned),
        }
    }

    fn dir(path: &str) -> ManifestEntry {
        ManifestEntry {
            kind: EntryKind::Dir,
            ..file(path, None)
        }
    }

    fn meta(baseline: Option<Baseline>) -> BackupMeta {
        let mut meta: BackupMeta = serde_json::from_value(serde_json::json!({
            "id": "01M6K2VA000000000000000000",
            "target": "01JB8Q2V9W0000000000000000",
            "target_id": "retail",
            "family": "mainline",
            "created_at": "2026-10-20T19:40:00Z",
            "captured_at": "2026-10-20T19:40:00Z",
            "kind": "manual",
            "trigger": null,
            "actor": "human",
            "reason": null,
            "client_version": "12.1.5.70391",
            "client_running": false,
            "files": 2,
            "dirs": 1,
            "symlinks": 0,
            "skipped": 0,
            "bytes": 2,
            "reflinked": "all",
            "unsettled": 0,
            "consistency": "stable",
            "integrity": "metadata_only",
            "verified_at": null,
            "baseline": null,
            "tool_version": "0.1.0",
            "source": null,
            "partial": false,
            "wtf_outside_client": false,
        }))
        .unwrap();
        meta.baseline = baseline;
        meta
    }

    #[test]
    fn hashes_present_compare_and_keep_their_baseline_and_absent_ones_are_recorded() {
        let names = WtfPathRedactor::for_paths(std::iter::empty());
        let hashed = Manifest {
            entries: vec![
                dir("Account"),
                file("Config.wtf", Some("aa")),
                file("b", Some("BB")),
            ],
        };
        let now = [None, Some("aa".to_owned()), Some("bb".to_owned())];
        for baseline in [Baseline::Capture, Baseline::Import, Baseline::Later] {
            let (outcome, mismatched, recorded) =
                judge(&meta(Some(baseline)), &hashed, &now, &names);
            assert_eq!(
                (outcome, mismatched, recorded),
                (VerifyOutcome::Verified { against: baseline }, 0, None),
                "a verified backup keeps its baseline"
            );
        }
        // Hashes recorded by a verify that died before its backup.json: a later baseline.
        let (outcome, ..) = judge(&meta(None), &hashed, &now, &names);
        assert_eq!(outcome.baseline(), Some(Baseline::Later));

        // One byte flipped: the first differing file named, nothing recorded.
        let flipped = [None, Some("aa".to_owned()), Some("cc".to_owned())];
        let (outcome, mismatched, recorded) =
            judge(&meta(Some(Baseline::Capture)), &hashed, &flipped, &names);
        assert_eq!(
            (outcome, mismatched, recorded),
            (
                VerifyOutcome::Mismatch {
                    files: 1,
                    first: "b".into()
                },
                1,
                None
            )
        );

        // No hashes: recorded, as a later baseline.
        let bare = Manifest {
            entries: vec![dir("Account"), file("Config.wtf", None), file("b", None)],
        };
        let (outcome, mismatched, recorded) = judge(&meta(None), &bare, &now, &names);
        assert_eq!(
            (outcome.clone(), mismatched),
            (VerifyOutcome::BaselineRecorded, 0)
        );
        assert_eq!(
            (outcome.integrity(), outcome.baseline()),
            (Integrity::Verified, Some(Baseline::Later))
        );
        let recorded: Vec<Option<String>> = recorded
            .unwrap()
            .entries
            .into_iter()
            .map(|e| e.sha256)
            .collect();
        assert_eq!(recorded, now);
    }

    #[test]
    fn a_backup_of_no_files_gets_a_baseline_too() {
        let names = WtfPathRedactor::for_paths(std::iter::empty());
        let empty = Manifest {
            entries: vec![dir("Account")],
        };
        let (outcome, ..) = judge(&meta(None), &empty, &[None], &names);
        assert_eq!(outcome, VerifyOutcome::BaselineRecorded);
        let (outcome, ..) = judge(&meta(Some(Baseline::Capture)), &empty, &[None], &names);
        assert_eq!(
            outcome,
            VerifyOutcome::Verified {
                against: Baseline::Capture
            }
        );
    }

    #[test]
    fn a_structure_or_content_difference_is_damage() {
        for outcome in [
            VerifyOutcome::Structure { first: None },
            VerifyOutcome::Mismatch {
                files: 1,
                first: "x".into(),
            },
        ] {
            assert_eq!(
                (outcome.integrity(), outcome.baseline()),
                (Integrity::Damaged, None)
            );
        }
    }
}
