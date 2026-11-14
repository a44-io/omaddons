//! `<cache>/errors.json`, the errors memo (M3-D20): what `errors` last observed on each
//! client, so `status --cached` can tell new errors and stale reads without opening a
//! BugGrabber file. Derived: written only from a whole observation (never a filtered
//! view), only when its encoding changed, and best effort. It holds observations only
//! (stamps, sessions, first-read baselines, counts), never a message, a stack or
//! locals: deleting it forgets which older errors were new.

use std::collections::BTreeMap;

use jiff::Timestamp;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::attribute::Confidence;
use super::buggrabber::{Origin, TimeEvidence};
use super::ledger::DbGeneration;
use super::locate::{Capture, Coverage};
use super::observe::{Observation, ReadState, SourceKind};
use super::suspect::Relation;
use crate::error::Result;
use crate::fsx::{Content, Op, ScopeTable, WriteSession, Writer, ensure_dir_all, mutate};
use crate::model::{Actor, ReleaseKey, TxId, TxKind};
use crate::state::paths::Paths;
use crate::state::store::{self, SchemaVersion, VersionStatus, Versioned};

/// The memo file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ErrorsMemo {
    /// By target key.
    #[serde(default)]
    pub targets: BTreeMap<String, TargetMemo>,
}

impl Versioned for ErrorsMemo {
    const KIND: &'static str = "errors_memo";
    /// 1.1: each addon's `links`, each database's `generation`, each client's
    /// `ledger_revision` and `projected_at` (regression guard, RG-17).
    const CURRENT: SchemaVersion = SchemaVersion::new(1, 1);
}

/// What the last `errors` run observed on one client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TargetMemo {
    /// `buggrabber` or `none`.
    pub capture: String,
    /// Account folders and files examined, and whether a bound cut the walk.
    pub coverage: MemoCoverage,
    /// Each `SavedVariables/` folder examined: its account ordinal and the fingerprint
    /// of its file names.
    pub listings: Vec<MemoListing>,
    /// Received reports: a count only.
    pub received: u64,
    pub databases: Vec<DbMemo>,
    /// The errors ledger's revision the links were projected from; omitted when none
    /// were. A projection from a lower revision than the memo already carries is never
    /// written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ledger_revision: Option<u64>,
    /// When the links were projected; omitted when none were. A restore committed
    /// later ends them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "crate::contract::schema::timestamp_opt")]
    pub projected_at: Option<Timestamp>,
}

/// How much of the client the walk saw.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MemoCoverage {
    pub truncated: bool,
    pub accounts: usize,
    pub files: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MemoListing {
    pub account: usize,
    pub fingerprint: String,
}

/// One database as last observed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DbMemo {
    /// Relative to the client folder (this machine only).
    pub path: String,
    pub account: usize,
    pub provider: String,
    /// The file's stamp as read (`FileStamp::fingerprint`).
    pub stamp: String,
    /// When the client last wrote it.
    #[schemars(schema_with = "crate::contract::schema::timestamp_opt")]
    pub written_at: Option<Timestamp>,
    pub state: ReadState,
    pub session: Option<u32>,
    /// Sessions above it are new (M3-D19); 0 after a reset.
    pub baseline: u32,
    /// Local entries per session (0: no session), each with its newest time evidence.
    pub sessions: BTreeMap<u32, SessionMemo>,
    pub entries: u64,
    pub occurrences: u64,
    /// Local entries per attributed addon, per session.
    pub addons: Vec<AddonMemo>,
    /// The ledger's generation of this database when its links were projected; omitted
    /// when unknown. A link of another generation is never shown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<DbGeneration>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SessionMemo {
    pub entries: u64,
    pub occurrences: u64,
    pub newest: TimeEvidence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AddonMemo {
    pub addon: String,
    pub managed: bool,
    pub kind: SourceKind,
    pub sessions: BTreeMap<u32, SessionMemo>,
    /// The folders attribution named, sorted ignoring case (`status`' `top[].folder`).
    /// Absent from a memo written before it, which reads as none.
    #[serde(default)]
    pub folders: Vec<String>,
    /// The changes before this addon's new errors in this database, best first, at most
    /// `MEMO_LINKS_PER_ADDON`: only `current` `high` and `medium` links. Omitted when
    /// none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<MemoLink>,
}

/// A link from a cluster of new errors to the change before it, with what it is about,
/// so `status` can check it still holds before showing it (design 7.3.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MemoLink {
    /// The cluster and its member fingerprints (at most 16).
    pub cluster: String,
    pub fingerprints: Vec<String>,
    /// The database generation the cluster appeared in.
    pub generation: DbGeneration,
    /// The newest session a member occurred in at this read, in the memo's convention
    /// (0: the entries carry no session).
    pub session: u32,
    pub tx: TxId,
    pub kind: TxKind,
    #[schemars(schema_with = "crate::contract::schema::timestamp")]
    pub at: Timestamp,
    /// The first addon the change moved (else the one the error is filed under), and
    /// the release that change installed for it.
    pub addon: String,
    pub release: Option<ReleaseKey>,
    pub relation: Relation,
    pub confidence: Confidence,
    pub algorithm: u32,
    /// `onset.by + SUSPECT_FRESH_MAX_AGE`: past it the link is no longer shown.
    #[schemars(schema_with = "crate::contract::schema::timestamp")]
    pub fresh_until_at: Timestamp,
}

/// What the errors ledger says of one client's databases, by their index in the
/// observation: each one's generation, and the links of each addon, by its name
/// (lowercased) and kind, the two things an [`AddonMemo`] is told apart by.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Projection {
    pub databases: Vec<DbProjection>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DbProjection {
    pub generation: Option<DbGeneration>,
    pub links: BTreeMap<(String, SourceKind), Vec<MemoLink>>,
}

/// The links `previous` held for `db`'s addons, kept only when the file is unchanged
/// (the same stamp): what was said about it still holds. Otherwise none.
fn carried(previous: Option<&DbMemo>, stamp: &str) -> (Option<DbGeneration>, Vec<AddonMemo>) {
    match previous.filter(|p| p.stamp == stamp) {
        Some(p) => (p.generation.clone(), p.addons.clone()),
        None => (None, Vec::new()),
    }
}

/// The memo of `observation`. A database this run could not read keeps what `previous`
/// last counted, never zero errors. `links` is what the ledger projected (design
/// 7.3.2); without it (a dismissal, or a run that could not have the ledger), a
/// database keeps its previous links only while its file is unchanged.
pub fn target_memo(
    observation: &Observation,
    previous: Option<&TargetMemo>,
    links: Option<&Projection>,
) -> TargetMemo {
    let databases = observation
        .databases
        .iter()
        .enumerate()
        .map(|(at, db)| {
            let before = previous.and_then(|p| p.databases.iter().find(|d| d.path == db.path));
            let stamp = db.stamp.fingerprint();
            let unread = matches!(db.state, ReadState::OverBudget | ReadState::Unreadable);
            if unread && let Some(before) = before {
                // What it last counted, with its links only while the file is unchanged.
                let (generation, kept) = carried(Some(before), &stamp);
                let mut addons = before.addons.clone();
                for addon in &mut addons {
                    addon.links = links_of(&kept, addon);
                }
                return DbMemo {
                    stamp,
                    written_at: db.written_at,
                    state: db.state,
                    baseline: db.baseline,
                    addons,
                    generation,
                    ..before.clone()
                };
            }
            let mut memo = DbMemo {
                path: db.path.clone(),
                account: db.account,
                provider: db.provider.clone(),
                stamp: stamp.clone(),
                written_at: db.written_at,
                state: db.state,
                session: db.session,
                baseline: db.baseline,
                sessions: BTreeMap::new(),
                entries: 0,
                occurrences: 0,
                addons: Vec::new(),
                generation: None,
            };
            for e in observation.local().filter(|e| e.database == at) {
                let session = e.entry.session.unwrap_or(0);
                let occurrences = u64::from(e.entry.counter);
                memo.entries += 1;
                memo.occurrences += occurrences;
                add(&mut memo.sessions, session, occurrences, e.evidence);
                let addon = match memo
                    .addons
                    .iter_mut()
                    .position(|a| a.addon.eq_ignore_ascii_case(&e.name) && a.kind == e.kind)
                {
                    Some(at) => &mut memo.addons[at],
                    None => {
                        memo.addons.push(AddonMemo {
                            addon: e.name.clone(),
                            managed: e.managed,
                            kind: e.kind,
                            sessions: BTreeMap::new(),
                            folders: Vec::new(),
                            links: Vec::new(),
                        });
                        let last = memo.addons.len() - 1;
                        &mut memo.addons[last]
                    }
                };
                add(&mut addon.sessions, session, occurrences, e.evidence);
                if let Some(folder) = &e.folder
                    && !addon.folders.iter().any(|f| f.eq_ignore_ascii_case(folder))
                {
                    addon.folders.push(folder.clone());
                }
            }
            for addon in &mut memo.addons {
                addon.folders.sort_by_key(|f| f.to_ascii_lowercase());
            }
            memo.addons.sort_by(|a, b| {
                (a.addon.to_ascii_lowercase(), a.kind).cmp(&(b.addon.to_ascii_lowercase(), b.kind))
            });
            match links {
                Some(projection) => {
                    let projected = projection.databases.get(at);
                    memo.generation = projected.and_then(|p| p.generation.clone());
                    for addon in &mut memo.addons {
                        let key = (addon.addon.to_ascii_lowercase(), addon.kind);
                        addon.links = projected
                            .and_then(|p| p.links.get(&key))
                            .cloned()
                            .unwrap_or_default();
                    }
                }
                None => {
                    let (generation, kept) = carried(before, &stamp);
                    memo.generation = generation;
                    for addon in &mut memo.addons {
                        addon.links = links_of(&kept, addon);
                    }
                }
            }
            memo
        })
        .collect();
    let coverage = match observation.coverage {
        Coverage::Complete => MemoCoverage {
            truncated: false,
            accounts: observation.listings.len(),
            files: observation.databases.len(),
        },
        Coverage::Truncated { accounts, files } => MemoCoverage {
            truncated: true,
            accounts,
            files,
        },
    };
    TargetMemo {
        capture: match observation.capture {
            Capture::BugGrabber => "buggrabber",
            Capture::None => "none",
        }
        .to_owned(),
        coverage,
        listings: observation
            .listings
            .iter()
            .map(|l| MemoListing {
                account: l.account,
                fingerprint: l.fingerprint.clone(),
            })
            .collect(),
        received: observation
            .entries
            .iter()
            .filter(|e| e.entry.origin == Origin::Received)
            .count() as u64,
        databases,
        // Set by the caller that projected the links, under the evidence lock; carried
        // over with the links it keeps otherwise.
        ledger_revision: previous.and_then(|p| p.ledger_revision),
        projected_at: previous.and_then(|p| p.projected_at),
    }
}

/// The links an earlier memo held for `addon` (by name ignoring case, and kind).
fn links_of(kept: &[AddonMemo], addon: &AddonMemo) -> Vec<MemoLink> {
    kept.iter()
        .find(|k| k.addon.eq_ignore_ascii_case(&addon.addon) && k.kind == addon.kind)
        .map(|k| k.links.clone())
        .unwrap_or_default()
}

fn add(
    sessions: &mut BTreeMap<u32, SessionMemo>,
    session: u32,
    occurrences: u64,
    at: TimeEvidence,
) {
    sessions
        .entry(session)
        .and_modify(|s| {
            s.entries += 1;
            s.occurrences += occurrences;
            s.newest = TimeEvidence::weakest(&s.newest, &at);
        })
        .or_insert(SessionMemo {
            entries: 1,
            occurrences,
            newest: at,
        });
}

/// The memo, when it reads; `writable` is false when a newer omaddons wrote it (then it
/// is neither believed nor overwritten).
pub fn load(paths: &Paths) -> (ErrorsMemo, bool) {
    match store::load::<ErrorsMemo>(&paths.errors_memo()) {
        Ok(Some(loaded)) if loaded.status != VersionStatus::Newer => (loaded.value, true),
        Ok(None) => (ErrorsMemo::default(), true),
        // Newer, and this build still decodes it.
        Ok(Some(_)) => (ErrorsMemo::default(), false),
        // Newer in a shape this build no longer decodes (`state_too_new`) is newer all the
        // same; only a corrupt file is replaced.
        // ubs:ignore — comparing an error code, not a secret
        Err(err) => (
            ErrorsMemo::default(),
            err.code != crate::error::ErrorCode::StateTooNew,
        ),
    }
}

/// Writes `memo` when its encoding differs from the file's, ringing the doorbell then
/// (M3-D4); whether it wrote. Best effort: a failure is not the command's.
pub fn store(paths: &Paths, memo: &ErrorsMemo, actor: Actor) -> bool {
    let Ok(json) = store::encode(memo) else {
        return false;
    };
    if std::fs::read(paths.errors_memo()).is_ok_and(|on_disk| on_disk == json) {
        return false;
    }
    if write(paths, &json).is_err() {
        return false;
    }
    crate::state::doorbell::ring(paths, "errors", actor);
    true
}

fn write(paths: &Paths, json: &[u8]) -> Result<()> {
    let scopes = ScopeTable::new(paths, &[], None)?;
    let mut s = WriteSession::untracked(Writer::Derived, &scopes, "errors-memo");
    ensure_dir_all(&mut s, paths.cache_dir())?;
    let op = Op::WriteFile {
        content: Content::Bytes(json),
        mode: Some(0o644),
    };
    mutate(&mut s, &paths.errors_memo(), op)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The memo is writable unless a newer omaddons wrote it, whether this build still
    /// decodes that file or not (a newer major may reshape a field): then it is neither
    /// believed nor overwritten. A corrupt or missing one is replaced.
    #[test]
    fn a_newer_memo_is_neither_believed_nor_writable_decoded_or_not() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let paths = Paths::with_roots(root, root.join("c"), root.join("s"), root.join("k"));
        assert_eq!(load(&paths), (ErrorsMemo::default(), true), "missing");
        std::fs::create_dir_all(paths.cache_dir()).unwrap();
        for (case, bytes, writable) in [
            ("corrupt", "{ not json", true),
            ("newer", r#"{"schema_version":"9.0","targets":{}}"#, false),
            (
                "newer, reshaped",
                r#"{"schema_version":"9.0","targets":"a shape this build cannot decode"}"#,
                false,
            ),
        ] {
            std::fs::write(paths.errors_memo(), bytes).unwrap();
            assert_eq!(
                load(&paths),
                (ErrorsMemo::default(), writable),
                "case {case}"
            );
        }
    }
}
