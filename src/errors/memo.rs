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

use super::buggrabber::{Origin, TimeEvidence};
use super::locate::{Capture, Coverage};
use super::observe::{Observation, ReadState, SourceKind};
use crate::error::Result;
use crate::fsx::{Content, Op, ScopeTable, WriteSession, Writer, ensure_dir_all, mutate};
use crate::model::Actor;
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
    const CURRENT: SchemaVersion = SchemaVersion::new(1, 0);
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
}

/// The memo of `observation`. A database this run could not read keeps what `previous`
/// last counted, never zero errors.
pub fn target_memo(observation: &Observation, previous: Option<&TargetMemo>) -> TargetMemo {
    let databases = observation
        .databases
        .iter()
        .enumerate()
        .map(|(at, db)| {
            let unread = matches!(db.state, ReadState::OverBudget | ReadState::Unreadable);
            if unread
                && let Some(before) =
                    previous.and_then(|p| p.databases.iter().find(|d| d.path == db.path))
            {
                return DbMemo {
                    stamp: db.stamp.fingerprint(),
                    written_at: db.written_at,
                    state: db.state,
                    baseline: db.baseline,
                    ..before.clone()
                };
            }
            let mut memo = DbMemo {
                path: db.path.clone(),
                account: db.account,
                provider: db.provider.clone(),
                stamp: db.stamp.fingerprint(),
                written_at: db.written_at,
                state: db.state,
                session: db.session,
                baseline: db.baseline,
                sessions: BTreeMap::new(),
                entries: 0,
                occurrences: 0,
                addons: Vec::new(),
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
    }
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
        Ok(Some(loaded)) if loaded.status == VersionStatus::Newer => (ErrorsMemo::default(), false),
        Ok(Some(loaded)) => (loaded.value, true),
        _ => (ErrorsMemo::default(), true),
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
