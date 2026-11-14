//! The repair journal (E5, M4-D31): `<state>/journal/<tx>.repair.jsonl`, format 1.0, the
//! contract between the commit, recovery, `journal::diagnose`, undo and older builds.
//!
//! One JSON object per line, each fsync'd before what it announces runs:
//!
//! - `prepared`: everything the repair will do, decided before it starts (the run, the
//!   actor, the clients, the resources, every fix and every primitive step with its
//!   endpoint predicates), so recovery and the history line need nothing else;
//! - `step` (`seq`): written ahead of primitive `seq`, in order;
//! - `done`: every primitive ran; `committed` follows once the history line is written;
//! - `rolled_back`: an in-process rollback undid the steps that ran, in place of `done`.
//!
//! A reader never acts on what it cannot fully read (M3-D45): a line out of that order, an
//! unknown line type or a complete line that does not decode makes the journal
//! unreadable, and a newer major format too new. A last line cut short is a crash
//! mid-append and is ignored: a torn `step` never ran.
//!
//! **Names.** [`list`] is the only parser of `.repair.jsonl` names. `paths::tx_named`
//! must never learn `.repair`: the folder listing would then read repair journals as
//! folder journals, and the orphan sweep would judge a repair's `<tx>.repair` snapshot
//! half by its `AddOns/` and `WTF.previous` alone and could delete a quarantine.

use std::path::{Path, PathBuf};

use jiff::Timestamp;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::engine::journal::Unrecoverable;
use crate::engine::repair_action::{FixEntry, Step};
use crate::engine::resource::{Resource, ResourceError, ResourceSet};
use crate::error::{Error, Result, Suggestion};
use crate::model::{Actor, PlanId, RunId, TargetKey, TxId, TxKind};
use crate::state::history::{self, HistoryEntry, RepairEntry, TxResult};
use crate::state::paths::{Paths, REPAIR_SUFFIX};
use crate::state::store::SchemaVersion;

/// The repair journal's format. A newer minor adds only optional fields an older build
/// may ignore; anything an older reader would act on wrongly bumps the major, which this
/// build never recovers.
pub const FORMAT: SchemaVersion = SchemaVersion::new(1, 0);

/// One line of a repair journal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RepairLine {
    Prepared(Box<RepairPrepared>),
    /// Written ahead of primitive `seq`.
    Step {
        seq: u32,
    },
    /// Every primitive ran.
    Done,
    /// An in-process rollback undid every step that ran.
    RolledBack,
    /// The history line is written; only cleanup is left.
    Committed,
}

/// The journal's first line: the whole repair, decided before anything runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RepairPrepared {
    /// The journal's format (`"1.0"`).
    pub format: SchemaVersion,
    pub tx: TxId,
    /// The command invocation, history's `run`.
    pub run: RunId,
    /// The doctor run directory's name, `<UTC time>Z__<ULID>`.
    pub doctor_run_id: String,
    pub actor: Actor,
    /// `--reason`, verbatim; `null` when none was given.
    pub reason: Option<String>,
    pub argv: Vec<String>,
    /// When the repair started.
    #[schemars(schema_with = "crate::contract::schema::timestamp")]
    pub at: Timestamp,
    /// The clients whose files a step touches, by key.
    pub targets: Vec<TargetKey>,
    /// Those clients' directories, canonical.
    pub client_dirs: Vec<ClientDirEntry>,
    /// The union of the steps' resources, in their text form.
    pub resources: ResourcesWire,
    pub fixes: Vec<FixEntry>,
    pub primitives: Vec<Step>,
    /// Where a rollback quarantines each file a write creates, by its step (lowering's
    /// choice, so a recovering command needs nothing but the journal). Omitted when no
    /// write creates a file.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rollbacks: Vec<RollbackTarget>,
    /// A repair, or the undo of one (E7), whose history line is then `kind: undo`.
    /// Omitted for a repair, so a repair's journal reads and writes as before.
    #[serde(default, skip_serializing_if = "RepairKind::is_repair")]
    pub kind: RepairKind,
    /// The repair-family transaction an undo reverts; required exactly on an undo
    /// ([`ParsedRepair::checked`] reads anything else as undecodable). Omitted otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reverts: Option<TxId>,
    /// The stored plan `apply` ran it from (D5's history `plan`), copied into its line by
    /// the commit and by recovery's roll-forward alike. Omitted otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<PlanId>,
}

/// What a repair-family transaction is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RepairKind {
    #[default]
    Repair,
    /// The undo of a repair-family transaction (`reverts`).
    Undo,
}

impl RepairKind {
    pub fn is_repair(&self) -> bool {
        *self == Self::Repair
    }
}

/// Where a rollback moves the file step `seq` created: never a delete.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RollbackTarget {
    pub seq: u32,
    pub to: PathBuf,
}

/// A client directory a repair touches, and its target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ClientDirEntry {
    pub dir: PathBuf,
    pub target: TargetKey,
}

/// A [`ResourceSet`] as the journal records it: each resource in its text form, sorted.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct ResourcesWire {
    pub reads: Vec<String>,
    pub writes: Vec<String>,
}

impl ResourcesWire {
    /// The text form of `set`; refused for a path that is not UTF-8, which lowering never
    /// admits.
    pub fn of(set: &ResourceSet) -> std::result::Result<Self, ResourceError> {
        let texts = |members: &std::collections::BTreeSet<Resource>| {
            members
                .iter()
                .map(Resource::text)
                .collect::<std::result::Result<Vec<String>, ResourceError>>()
        };
        Ok(Self {
            reads: texts(set.reads())?,
            writes: texts(set.writes())?,
        })
    }

    /// The set back, each resource as recorded, never resolved again.
    pub fn to_set(&self) -> std::result::Result<ResourceSet, ResourceError> {
        let mut set = ResourceSet::new();
        for text in &self.reads {
            set.read(Resource::parse(text)?);
        }
        for text in &self.writes {
            set.write(Resource::parse(text)?);
        }
        Ok(set)
    }
}

// ---------------------------------------------------------------------------------------
// Reading.

/// How far a repair got, as its journal says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RepairPhase {
    /// No `prepared` line: nothing ran.
    Unprepared,
    Prepared,
    /// Step lines `1..=k` were written; primitive `k` may or may not have run.
    Stepped(u32),
    Done,
    Committed,
    RolledBack,
}

/// A repair journal as read back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedRepair {
    pub prepared: Option<RepairPrepared>,
    pub phase: RepairPhase,
    /// Step lines recorded, `1..=steps` in order.
    pub steps: u32,
    /// Complete lines this build does not read: an unknown line type, a line out of the
    /// journal's order, JSON that does not decode, or a line that is not JSON before the
    /// last. Recovery never acts on a journal that has one (M3-D45).
    pub unknown_lines: usize,
    /// The last line was cut short by a crash mid-append, and was ignored.
    pub torn_tail: bool,
    /// Complete lines, read or not; the torn tail is not one.
    pub lines: usize,
    /// The first `prepared` line's own format, read even when the line does not decode.
    pub format: Option<SchemaVersion>,
}

impl ParsedRepair {
    /// Why recovery must leave this journal as it is, when it must: a newer major format,
    /// or a complete line it cannot read (a `prepared` line that does not decode is one).
    /// A journal that is nothing but a torn tail is not: nothing ran.
    pub fn unrecoverable(&self) -> Option<Unrecoverable> {
        if self
            .format
            .is_some_and(|format| format.major > FORMAT.major)
        {
            Some(Unrecoverable::TooNew)
        } else if self.unknown_lines > 0 || (self.prepared.is_none() && self.lines > 0) {
            Some(Unrecoverable::Unreadable)
        } else {
            None
        }
    }
}

/// Parses a repair journal, never failing: what it cannot read is counted for
/// [`ParsedRepair::unrecoverable`]. The order the lines must come in:
///
/// - the first complete line is `prepared`, and no other line is;
/// - `step` lines run `1, 2, …` without a gap, never past the primitive count;
/// - `done` comes after exactly as many step lines as primitives (none for none);
/// - `committed` comes only after `done`; `rolled_back` only after `prepared` or a step
///   line, never after `done`;
/// - nothing follows `committed` or `rolled_back`.
///
/// Any other line is an unknown line. A last line without its newline, or one that is not
/// JSON, is a torn tail and is ignored. Unknown fields inside a known line are ignored, so
/// a newer minor can add optional ones.
pub fn parse(text: &str) -> ParsedRepair {
    let mut parsed = ParsedRepair {
        prepared: None,
        phase: RepairPhase::Unprepared,
        steps: 0,
        unknown_lines: 0,
        torn_tail: false,
        lines: 0,
        format: None,
    };
    let complete = text.ends_with('\n');
    let lines: Vec<&str> = text.lines().collect();
    for (i, raw) in lines.iter().enumerate() {
        let last = i + 1 == lines.len();
        if last && !complete {
            parsed.torn_tail = true;
            break;
        }
        let value: serde_json::Value = match serde_json::from_str(raw) {
            Ok(value) => value,
            Err(_) if last => {
                parsed.torn_tail = true;
                break;
            }
            Err(_) => {
                parsed.lines += 1;
                parsed.unknown_lines += 1;
                continue;
            }
        };
        parsed.lines += 1;
        let first = parsed.lines == 1;
        if first && value.get("type").and_then(serde_json::Value::as_str) == Some("prepared") {
            parsed.format = value
                .get("format")
                .and_then(serde_json::Value::as_str)
                .and_then(|format| format.parse().ok());
        }
        let Ok(line) = serde_json::from_value::<RepairLine>(value) else {
            parsed.unknown_lines += 1;
            continue;
        };
        if !parsed.advance(line, first) {
            parsed.unknown_lines += 1;
        }
    }
    parsed
}

impl ParsedRepair {
    /// Applies `line` when it comes where the journal's order allows it.
    fn advance(&mut self, line: RepairLine, first: bool) -> bool {
        let primitives = self
            .prepared
            .as_ref()
            .map_or(0, |prepared| prepared.primitives.len());
        let next = match (line, self.phase) {
            (RepairLine::Prepared(prepared), RepairPhase::Unprepared) if first => {
                self.prepared = Some(*prepared);
                RepairPhase::Prepared
            }
            (RepairLine::Step { seq }, RepairPhase::Prepared | RepairPhase::Stepped(_))
                if seq == self.steps + 1 && (seq as usize) <= primitives =>
            {
                self.steps = seq;
                RepairPhase::Stepped(seq)
            }
            (RepairLine::Done, RepairPhase::Prepared | RepairPhase::Stepped(_))
                if self.steps as usize == primitives =>
            {
                RepairPhase::Done
            }
            (RepairLine::Committed, RepairPhase::Done) => RepairPhase::Committed,
            (RepairLine::RolledBack, RepairPhase::Prepared | RepairPhase::Stepped(_)) => {
                RepairPhase::RolledBack
            }
            _ => return false,
        };
        self.phase = next;
        true
    }
}

/// Every `<state>/journal/<ULID>.repair.jsonl`, by transaction, sorted. A missing journal
/// directory lists nothing; one that cannot be listed is an `io` error, and the caller
/// fails closed. Nothing else lists repair journals.
pub fn list(paths: &Paths) -> Result<Vec<(TxId, PathBuf)>> {
    let dir = paths.journal_dir();
    let listing = match std::fs::read_dir(&dir) {
        Ok(listing) => listing,
        // ubs:ignore — an io::ErrorKind, not a secret
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => {
            return Err(Error::io(
                &dir,
                &e,
                Suggestion::command(["omaddons", "status"]),
            ));
        }
    };
    let suffix = format!("{REPAIR_SUFFIX}.jsonl");
    let mut found: Vec<(TxId, PathBuf)> = listing
        .filter_map(std::result::Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .filter_map(|entry| {
            let path = entry.path();
            let tx = path
                .file_name()?
                .to_str()?
                .strip_suffix(&suffix)?
                .parse()
                .ok()?;
            Some((tx, path))
        })
        .collect();
    found.sort();
    Ok(found)
}

impl ParsedRepair {
    /// This journal as recovery may act on it (E14): a `prepared` line that decodes but
    /// breaks the activation rules ([`barrier::validate`](crate::engine::barrier::validate))
    /// is read as one that does not decode, so recovery leaves the journal whole and its
    /// fences close (M3-D45: fail closed on what cannot be fully read). Every reader that
    /// acts on a repair journal reads it through this.
    pub fn checked(mut self, paths: &Paths) -> Self {
        // An undo names what it reverts, and only an undo does (E7).
        let misnamed = |prepared: &RepairPrepared| {
            (prepared.kind == RepairKind::Undo) != prepared.reverts.is_some()
        };
        if self.prepared.as_ref().is_some_and(|prepared| {
            misnamed(prepared) || crate::engine::barrier::validate(prepared, paths).is_err()
        }) {
            self.prepared = None;
            self.unknown_lines += 1;
        }
        self
    }
}

/// The repair whose client-side half a `<meta>/snapshots/` entry is: exactly a
/// transaction id as it is written, then [`REPAIR_SUFFIX`] (`<ULID>.repair`), nothing
/// else. With [`list`], the only parser of repair names (M4-D31).
pub fn half_tx(name: &str) -> Option<TxId> {
    let id = name.strip_suffix(REPAIR_SUFFIX)?;
    id.parse::<TxId>().ok().filter(|tx| tx.to_string() == id)
}

/// Reads and parses one repair journal of `paths`, [checked](ParsedRepair::checked).
pub fn read(paths: &Paths, path: &Path) -> Result<ParsedRepair> {
    let bytes = std::fs::read(path)
        .map_err(|e| Error::io(path, &e, Suggestion::command(["omaddons", "status"])))?;
    Ok(parse(&String::from_utf8_lossy(&bytes)).checked(paths))
}

/// What a repair journal's first line tells of what the repair may have changed (E14):
/// the whole `prepared` line, or why this build cannot know.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreparedRead {
    /// The first complete line is a `prepared` line of a format this build reads. A
    /// later line that does not read leaves it decoded: recovery keeps such a journal
    /// whole (M3-D45), and what it changed is still known.
    Decoded(Box<RepairPrepared>),
    /// The first complete line is not a `prepared` line this build decodes, or (read
    /// through [`ParsedRepair::checked`]) it breaks the activation rules.
    Undecodable,
    /// The first line declares a newer major format.
    TooNew,
    /// No complete line: nothing ran.
    Absent,
}

impl ParsedRepair {
    /// [`PreparedRead`] of this journal: a newer major wins over a line that happens to
    /// decode, as in [`ParsedRepair::unrecoverable`].
    pub fn prepared_read(&self) -> PreparedRead {
        if self
            .format
            .is_some_and(|format| format.major > FORMAT.major)
        {
            PreparedRead::TooNew
        } else if let Some(prepared) = &self.prepared {
            PreparedRead::Decoded(Box::new(prepared.clone()))
        } else if self.lines > 0 {
            PreparedRead::Undecodable
        } else {
            PreparedRead::Absent
        }
    }
}

/// Reads one repair journal of `paths` for its [`PreparedRead`]: a `prepared` line that
/// breaks the activation rules is [`PreparedRead::Undecodable`].
pub fn read_prepared(paths: &Paths, path: &Path) -> Result<PreparedRead> {
    Ok(read(paths, path)?.prepared_read())
}

// ---------------------------------------------------------------------------------------
// The history line.

/// The history line a repair writes, built from its `prepared` line alone, so the commit
/// and recovery's roll-forward append the same bytes: target-less, its clients in
/// `targets`, no folder `ops`, and each primitive as a `repairs[]` entry. An undo's line
/// is `kind: undo` with `reverts`, its `repairs[]` the inverses (E7).
pub fn history_line(prepared: &RepairPrepared) -> HistoryEntry {
    let target_of = |path: &Path| {
        prepared
            .client_dirs
            .iter()
            .find(|client| path.starts_with(&client.dir))
            .map(|client| client.target)
    };
    let kind = match prepared.kind {
        RepairKind::Repair => TxKind::Repair,
        RepairKind::Undo => TxKind::Undo,
    };
    let entry = HistoryEntry {
        schema_version: history::CURRENT,
        tx: prepared.tx,
        kind,
        at: prepared.at,
        target: None,
        targets: prepared.targets.clone(),
        actor: prepared.actor,
        reason: prepared.reason.clone(),
        argv: prepared.argv.clone(),
        ops: Vec::new(),
        result: TxResult::Committed,
        reverts: prepared.reverts,
        sv_deferred: false,
        sv_restored: None,
        created_addons_dir: false,
        run: Some(prepared.run),
        backup: None,
        plan: prepared.plan,
        doctor_run_id: Some(prepared.doctor_run_id.clone()),
        repairs: prepared
            .primitives
            .iter()
            .map(|step| RepairEntry::from_step(step, target_of))
            .collect(),
        // A repair writes no client record, so it moves no release.
        transitions: Vec::new(),
    };
    debug_assert!(entry.well_formed(), "a repair's history line reads back");
    debug_assert!(
        !entry.repairs.iter().any(|repair| {
            [
                Some(&repair.path),
                repair.to.as_ref(),
                repair.backup.as_ref(),
            ]
            .into_iter()
            .flatten()
            .any(|path| below_wtf(path, &prepared.client_dirs))
        }),
        "lowering refuses every path below a client's WTF/"
    );
    entry
}

/// Whether `path` is below the `WTF/` of one of `clients`.
fn below_wtf(path: &Path, clients: &[ClientDirEntry]) -> bool {
    clients.iter().any(|client| {
        path.strip_prefix(&client.dir)
            .ok()
            .and_then(|rest| rest.iter().next())
            .and_then(|first| first.to_str())
            .is_some_and(|first| first.eq_ignore_ascii_case("WTF"))
    })
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use serde_json::Value;

    use super::*;
    use crate::engine::repair::{LowerCtx, PlannedFix, lower};
    use crate::engine::repair_action::{
        Endpoint, Endpoints, Fix, Guard, Identity, Mode, Primitive, QuarantineRoot, Stamp,
        SuspendedArtifact,
    };
    use crate::engine::testing::{REPAIR_RUN_ID, RepairHome, repair_home};
    use crate::state::history::Reach;

    const NESTED: &str = "fm-external_artifacts-nested-addon-folder";
    const SKILL: &str = "fm-plugins-skill-version-drift";
    const PENDING: &str = "fm-concurrency_primitives-journal-pending";

    fn tx() -> TxId {
        "01JB8Q2VA00000000000000003".parse().unwrap()
    }

    #[test]
    fn half_tx_recognizes_exactly_ulid_dot_repair() {
        let id = tx().to_string();
        let lower = id.to_lowercase();
        for (case, name, want) in [
            ("ulid_dot_repair", format!("{id}.repair"), Some(tx())),
            ("bare_ulid", id.clone(), None),
            ("wtf_suffix", format!("{id}.wtf"), None),
            ("journal_name", format!("{id}.repair.jsonl"), None),
            ("not_an_id", "x.repair".to_owned(), None),
            ("suffix_alone", ".repair".to_owned(), None),
            ("suffix_in_another_case", format!("{id}.Repair"), None),
            ("id_in_lowercase", format!("{lower}.repair"), None),
            ("trailing_slash", format!("{id}.repair/"), None),
        ] {
            assert_eq!(half_tx(&name), want, "{case}: half_tx({name:?})");
        }
    }

    fn planned(fm: &'static str, fix: Fix) -> PlannedFix {
        PlannedFix {
            fm: Cow::Borrowed(fm),
            fix,
        }
    }

    fn flatten(home: &RepairHome) -> PlannedFix {
        planned(
            NESTED,
            Fix::Flatten {
                inner: home.addons("Wrap/Inner"),
                outer: home.addons("Wrap"),
            },
        )
    }

    fn write(home: &RepairHome) -> PlannedFix {
        planned(
            SKILL,
            Fix::Write {
                path: home.skill_file(),
                content: b"new guide\n".to_vec(),
                mode: Mode::new(0o644).unwrap(),
            },
        )
    }

    fn delegate(home: &RepairHome) -> PlannedFix {
        planned(
            PENDING,
            Fix::Delegate {
                what: crate::engine::repair_action::Delegation {
                    tx: home.stuck(),
                    family: crate::engine::repair_action::JournalFamily::Folder,
                    phase: crate::engine::repair_action::JournalPhase::Prepared,
                    direction: crate::engine::repair_action::Direction::RollBack,
                },
            },
        )
    }

    /// The `prepared` line the commit would write for `fixes`, lowered on `home`.
    fn prepared(home: &RepairHome, fixes: &[PlannedFix]) -> RepairPrepared {
        let ctx = LowerCtx {
            paths: &home.paths,
            tx: tx(),
            run: &home.run,
            clients: &home.planned,
            skill_dirs: &home.skill_dirs,
            scopes: &home.scopes,
            mount_id: &crate::fsx::mount_id,
        };
        let lowered = lower(&ctx, fixes).unwrap();
        RepairPrepared {
            format: FORMAT,
            tx: tx(),
            run: "01JB8Q2VA00000000000000004".parse().unwrap(),
            doctor_run_id: REPAIR_RUN_ID.to_owned(),
            actor: Actor::Human,
            reason: None,
            argv: ["omaddons", "doctor", "fix"].map(str::to_owned).to_vec(),
            at: "2026-10-30T18:03:40Z".parse().unwrap(),
            targets: lowered.targets.clone(),
            client_dirs: lowered
                .client_dirs
                .iter()
                .map(|(dir, target)| ClientDirEntry {
                    dir: dir.clone(),
                    target: *target,
                })
                .collect(),
            resources: ResourcesWire::of(&lowered.resources).unwrap(),
            fixes: lowered.fixes.clone(),
            primitives: lowered.steps.iter().map(|s| s.step.clone()).collect(),
            rollbacks: lowered
                .steps
                .iter()
                .filter_map(|s| {
                    s.rollback_to.clone().map(|to| RollbackTarget {
                        seq: s.step.seq,
                        to,
                    })
                })
                .collect(),
            kind: RepairKind::Repair,
            reverts: None,
            plan: None,
        }
    }

    /// One journal line, as the commit appends it.
    fn line(line: &RepairLine) -> String {
        format!("{}\n", serde_json::to_string(line).unwrap())
    }

    fn step(seq: u32) -> String {
        line(&RepairLine::Step { seq })
    }

    #[test]
    fn every_line_round_trips() {
        let home = repair_home();
        let cases = [
            (
                "prepared_flatten",
                RepairLine::Prepared(Box::new(prepared(&home, &[flatten(&home)]))),
            ),
            (
                "prepared_write",
                RepairLine::Prepared(Box::new(prepared(&home, &[write(&home)]))),
            ),
            ("step_1", RepairLine::Step { seq: 1 }),
            ("done", RepairLine::Done),
            ("rolled_back", RepairLine::RolledBack),
            ("committed", RepairLine::Committed),
        ];
        for (case, want) in cases {
            let text = serde_json::to_string(&want).unwrap();
            let back: RepairLine =
                serde_json::from_str(&text).unwrap_or_else(|err| panic!("{case}: {err}\n{text}")); // ubs:ignore — fails the test
            assert_eq!(back, want, "{case}: {text}");
        }
        assert_eq!(step(2), "{\"type\":\"step\",\"seq\":2}\n");
        for (marker, want) in [
            (RepairLine::Done, "done"),
            (RepairLine::RolledBack, "rolled_back"),
            (RepairLine::Committed, "committed"),
        ] {
            assert_eq!(line(&marker), format!("{{\"type\":\"{want}\"}}\n"));
        }
    }

    /// `value` with what differs between machines and runs replaced: the home's paths by
    /// placeholders, and identities (whose `born_ns` only some filesystems record),
    /// stamps and the client's key by markers.
    fn redacted(home: &RepairHome, value: &Value) -> String {
        fn walk(value: &mut Value) {
            match value {
                Value::Object(map) => {
                    for (key, child) in map.iter_mut() {
                        match key.as_str() {
                            "identity" => *child = Value::from("<identity>"),
                            "stamp" => *child = Value::from("<stamp>"),
                            _ => walk(child),
                        }
                    }
                }
                Value::Array(items) => items.iter_mut().for_each(walk),
                _ => {}
            }
        }
        let mut value = value.clone();
        walk(&mut value);
        let text = serde_json::to_string_pretty(&value).unwrap();
        let shown = |path: &Path| path.display().to_string();
        text.replace(&shown(&home.client.meta_dir()), "<meta>")
            .replace(&shown(home.client.client_dir()), "<client dir>")
            .replace(&shown(&home.run.dir), "<run>")
            .replace(&home.key.to_string(), "<key>")
    }

    #[test]
    fn prepared_serializes_to_the_documented_wire_form() {
        let home = repair_home();
        let line = RepairLine::Prepared(Box::new(prepared(&home, &[flatten(&home)])));
        let compact = serde_json::to_string(&line).unwrap();
        assert!(
            compact.starts_with(
                "{\"type\":\"prepared\",\"format\":\"1.0\",\"tx\":\"01JB8Q2VA00000000000000003\",\"run\":"
            ),
            "{compact}"
        );
        assert!(
            compact.contains("\"reason\":null"),
            "a missing reason is null: {compact}"
        );
        let value = serde_json::to_value(&line).unwrap();
        insta::assert_snapshot!("repair_prepared_wire_form", redacted(&home, &value));
    }

    /// A journal of `prepared` then `rest`.
    fn journal(prepared: &RepairPrepared, rest: &[String]) -> String {
        let mut text = line(&RepairLine::Prepared(Box::new(prepared.clone())));
        for line in rest {
            text.push_str(line);
        }
        text
    }

    #[test]
    fn parse_reads_each_phase() {
        let home = repair_home();
        let two = prepared(&home, &[flatten(&home)]);
        assert_eq!(two.primitives.len(), 2, "the flatten has two renames");
        let none = prepared(&home, &[delegate(&home)]);
        assert!(none.primitives.is_empty(), "a delegation has none");
        let p = journal(&two, &[]);
        let cut = p.trim_end_matches('\n');
        let (done, committed, rolled_back) = (
            line(&RepairLine::Done),
            line(&RepairLine::Committed),
            line(&RepairLine::RolledBack),
        );
        let cases: Vec<(&str, String, RepairPhase, usize, bool)> = vec![
            ("empty", String::new(), RepairPhase::Unprepared, 0, false),
            (
                "torn_prepared",
                cut[..cut.len() / 2].to_owned(),
                RepairPhase::Unprepared,
                0,
                true,
            ),
            ("prepared", p.clone(), RepairPhase::Prepared, 1, false),
            (
                "stepped_1",
                journal(&two, &[step(1)]),
                RepairPhase::Stepped(1),
                2,
                false,
            ),
            (
                "stepped_2",
                journal(&two, &[step(1), step(2)]),
                RepairPhase::Stepped(2),
                3,
                false,
            ),
            (
                "done",
                journal(&two, &[step(1), step(2), done.clone()]),
                RepairPhase::Done,
                4,
                false,
            ),
            (
                "committed",
                journal(&two, &[step(1), step(2), done.clone(), committed.clone()]),
                RepairPhase::Committed,
                5,
                false,
            ),
            (
                "rolled_back_after_prepared",
                journal(&two, std::slice::from_ref(&rolled_back)),
                RepairPhase::RolledBack,
                2,
                false,
            ),
            (
                "rolled_back_after_step",
                journal(&two, &[step(1), rolled_back.clone()]),
                RepairPhase::RolledBack,
                3,
                false,
            ),
            (
                "zero_primitives_done",
                journal(&none, &[done.clone(), committed.clone()]),
                RepairPhase::Committed,
                3,
                false,
            ),
        ];
        for (case, text, phase, lines, torn) in cases {
            let parsed = parse(&text);
            let got = (
                parsed.phase,
                parsed.lines,
                parsed.torn_tail,
                parsed.unknown_lines,
            );
            assert_eq!(got, (phase, lines, torn, 0), "{case}:\n{text}");
            assert_eq!(parsed.unrecoverable(), None, "{case}:\n{text}");
            assert_eq!(
                parsed.prepared.is_some(),
                phase != RepairPhase::Unprepared,
                "{case}: a prepared line read exactly when one is whole"
            );
            assert_eq!(parse(&text), parsed, "{case}: parsing is idempotent");
        }
        let parsed = parse(&journal(&two, &[step(1), step(2)]));
        assert_eq!(parsed.steps, 2);
        assert_eq!(parsed.format, Some(FORMAT));
        assert_eq!(
            parsed.prepared,
            Some(two),
            "the prepared line reads back whole"
        );
    }

    #[test]
    fn prepared_read_decides_what_a_first_line_tells() {
        let home = repair_home();
        let two = prepared(&home, &[flatten(&home)]);
        let p = journal(&two, &[]);
        let decoded = PreparedRead::Decoded(Box::new(two.clone()));
        let too_new = p.replacen("\"format\":\"1.0\"", "\"format\":\"9.0\"", 1);
        let newer_minor = p.replacen("\"format\":\"1.0\"", "\"format\":\"1.7\"", 1);
        let cases: Vec<(&str, String, PreparedRead)> = vec![
            ("prepared_alone", p.clone(), decoded.clone()),
            (
                "prepared_then_steps",
                journal(&two, &[step(1)]),
                decoded.clone(),
            ),
            (
                "a_bad_later_line",
                journal(&two, &["not json\n".to_owned(), step(1)]),
                decoded.clone(),
            ),
            ("newer_major", too_new.clone(), PreparedRead::TooNew),
            (
                "newer_major_with_steps",
                format!("{too_new}{}", step(1)),
                PreparedRead::TooNew,
            ),
            (
                "newer_minor",
                newer_minor.clone(),
                PreparedRead::Decoded(Box::new(parse(&newer_minor).prepared.unwrap())),
            ),
            (
                "first_line_not_json",
                format!("not json\n{p}"),
                PreparedRead::Undecodable,
            ),
            ("first_line_a_step", step(1), PreparedRead::Undecodable),
            (
                "prepared_missing_a_field",
                "{\"type\":\"prepared\",\"format\":\"1.0\"}\n".to_owned(),
                PreparedRead::Undecodable,
            ),
            ("empty", String::new(), PreparedRead::Absent),
            (
                "torn_prepared",
                p.trim_end_matches('\n').to_owned(),
                PreparedRead::Absent,
            ),
        ];
        for (case, text, want) in cases {
            assert_eq!(parse(&text).prepared_read(), want, "{case}:\n{text}");
        }
    }

    /// A `prepared` line that decodes but breaks the activation rules is read, by every
    /// reader that acts on a journal, as one that does not decode (E14): recovery leaves it
    /// whole, and its fences close.
    #[test]
    fn read_prepared_treats_a_malformed_activation_as_undecodable() {
        let home = repair_home();
        let well_formed = prepared(&home, &[flatten(&home)]);
        let mut malformed = well_formed.clone();
        let last = malformed.primitives.last_mut().unwrap();
        let Primitive::RenameNoReplace { activation, .. } = &mut last.op else {
            panic!("{:?}", last.op); // ubs:ignore — fails the test
        };
        // A flatten's rename flagged as an activation: not from a provisional name.
        *activation = true;
        let dir = home.paths.journal_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let cases = [
            ("well_formed", &well_formed, true),
            ("malformed", &malformed, false),
        ];
        for (case, p, decodes) in cases {
            let path = dir.join(format!("{case}.repair.jsonl"));
            let text = journal(p, &[step(1)]);
            std::fs::write(&path, &text).unwrap();
            assert_eq!(
                parse(&text).prepared_read(),
                PreparedRead::Decoded(Box::new(p.clone())),
                "{case}: the raw parse decodes either"
            );
            let read_back = read(&home.paths, &path).unwrap();
            let got = read_prepared(&home.paths, &path).unwrap();
            if decodes {
                assert_eq!(got, PreparedRead::Decoded(Box::new(p.clone())), "{case}");
                assert_eq!(read_back.unrecoverable(), None, "{case}");
            } else {
                assert_eq!(got, PreparedRead::Undecodable, "{case}");
                assert_eq!(
                    read_back.unrecoverable(),
                    Some(Unrecoverable::Unreadable),
                    "{case}: recovery leaves it"
                );
                assert_eq!(read_back.prepared, None, "{case}");
            }
        }
    }

    /// A repair's journal names no kind and reverts nothing: it reads without them, and
    /// writes the same bytes as before E7.
    #[test]
    fn prepared_kind_and_reverts_default_for_repair_journals() {
        let home = repair_home();
        let p = prepared(&home, &[write(&home)]);
        let text = serde_json::to_string(&RepairLine::Prepared(Box::new(p.clone()))).unwrap();
        assert!(
            !text.contains("\"kind\":\"repair\"") && !text.contains("\"reverts\""),
            "{text}"
        );
        let mut value: Value = serde_json::from_str(&text).unwrap();
        let object = value.as_object_mut().unwrap();
        object.remove("kind");
        object.remove("reverts");
        let without: RepairLine = serde_json::from_value(value).unwrap();
        let RepairLine::Prepared(read_back) = &without else {
            panic!("{without:?}"); // ubs:ignore — fails the test
        };
        assert_eq!(read_back.kind, RepairKind::Repair);
        assert_eq!(read_back.reverts, None);
        assert_eq!(
            serde_json::to_string(&without).unwrap(),
            text,
            "re-serializes byte for byte"
        );
    }

    /// `kind` and `reverts` go together: either alone reads as a line this build cannot
    /// act on, and recovery leaves the journal whole.
    #[test]
    fn a_kind_without_reverts_is_unreadable() {
        let home = repair_home();
        let base = prepared(&home, &[write(&home)]);
        let dir = home.paths.journal_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let cases = [
            (
                "undo_without_reverts",
                RepairPrepared {
                    kind: RepairKind::Undo,
                    reverts: None,
                    ..base.clone()
                },
                false,
            ),
            (
                "reverts_without_undo",
                RepairPrepared {
                    kind: RepairKind::Repair,
                    reverts: Some(TxId::new()),
                    ..base.clone()
                },
                false,
            ),
            (
                "undo_with_reverts",
                RepairPrepared {
                    kind: RepairKind::Undo,
                    reverts: Some(TxId::new()),
                    ..base.clone()
                },
                true,
            ),
        ];
        for (case, p, decodes) in cases {
            let path = dir.join(format!("{case}.repair.jsonl"));
            std::fs::write(&path, journal(&p, &[step(1)])).unwrap();
            let got = read_prepared(&home.paths, &path).unwrap();
            if decodes {
                assert_eq!(got, PreparedRead::Decoded(Box::new(p.clone())), "{case}");
            } else {
                assert_eq!(got, PreparedRead::Undecodable, "{case}");
                assert_eq!(
                    read(&home.paths, &path).unwrap().unrecoverable(),
                    Some(Unrecoverable::Unreadable),
                    "{case}: recovery leaves it"
                );
            }
        }
    }

    /// An undo journal rolls forward into an `undo` line: target-less, reverting its
    /// repair, its inverses as `repairs[]`, a line history reads back.
    #[test]
    fn an_undo_journal_rolls_forward_into_an_undo_line() {
        let home = repair_home();
        let reverted = TxId::new();
        let p = RepairPrepared {
            kind: RepairKind::Undo,
            reverts: Some(reverted),
            ..prepared(&home, &[write(&home)])
        };
        let line = history_line(&p);
        assert!(line.well_formed(), "{line:?}");
        assert_eq!(line.kind, TxKind::Undo);
        assert_eq!(line.reverts, Some(reverted));
        assert_eq!(line.target, None);
        assert_eq!(line.repairs.len(), p.primitives.len());
        assert_eq!(
            line.doctor_run_id.as_deref(),
            Some(p.doctor_run_id.as_str())
        );
        assert_eq!(
            history_line(&prepared(&home, &[write(&home)])).kind,
            TxKind::Repair
        );
    }

    /// `prepared.plan` (E7 s7): absent, the line has no `plan` and the `prepared` bytes
    /// are as before; present, the line's `plan` is it.
    #[test]
    fn prepared_plan_is_optional_and_copied_into_the_line() {
        let home = repair_home();
        let without = prepared(&home, &[write(&home)]);
        let text = serde_json::to_string(&RepairLine::Prepared(Box::new(without.clone()))).unwrap();
        assert!(!text.contains("\"plan\""), "{text}");
        assert_eq!(history_line(&without).plan, None);
        let plan = PlanId::new();
        let with = RepairPrepared {
            kind: RepairKind::Undo,
            reverts: Some(TxId::new()),
            plan: Some(plan),
            ..without
        };
        let text = serde_json::to_string(&RepairLine::Prepared(Box::new(with.clone()))).unwrap();
        assert!(text.contains(&format!("\"plan\":\"{plan}\"")), "{text}");
        assert_eq!(history_line(&with).plan, Some(plan));
        let parsed = parse(&format!("{text}\n"));
        assert_eq!(
            parsed.prepared.map(|p| p.plan),
            Some(Some(plan)),
            "it reads back"
        );
    }

    #[test]
    fn parse_counts_what_it_cannot_read() {
        let home = repair_home();
        let two = prepared(&home, &[flatten(&home)]);
        let p = journal(&two, &[]);
        let (done, committed, rolled_back) = (
            line(&RepairLine::Done),
            line(&RepairLine::Committed),
            line(&RepairLine::RolledBack),
        );
        let unreadable: Vec<(&str, String)> = vec![
            (
                "unknown_type",
                journal(&two, &["{\"type\":\"swapped\"}\n".to_owned()]),
            ),
            ("step_out_of_order", journal(&two, &[step(2), step(1)])),
            ("step_gap", journal(&two, &[step(2)])),
            (
                "step_beyond_primitives",
                journal(&two, &[step(1), step(2), step(3)]),
            ),
            (
                "done_before_all_steps",
                journal(&two, &[step(1), done.clone()]),
            ),
            (
                "committed_without_done",
                journal(&two, &[step(1), step(2), committed.clone()]),
            ),
            (
                "rolled_back_after_done",
                journal(&two, &[step(1), step(2), done.clone(), rolled_back.clone()]),
            ),
            (
                "line_after_committed",
                journal(
                    &two,
                    &[
                        step(1),
                        step(2),
                        done.clone(),
                        committed.clone(),
                        done.clone(),
                    ],
                ),
            ),
            ("second_prepared", format!("{p}{p}")),
            (
                "garbage_middle_line",
                journal(&two, &["not json\n".to_owned(), step(1)]),
            ),
            ("prepared_not_first", format!("{}{p}", step(1))),
            ("step_first", step(1)),
            (
                "prepared_that_does_not_decode",
                "{\"type\":\"prepared\",\"format\":\"1.0\",\"tx\":\"nope\"}\n".to_owned(),
            ),
        ];
        for (case, text) in unreadable {
            let parsed = parse(&text);
            assert!(
                parsed.unknown_lines > 0 || parsed.prepared.is_none(),
                "{case}: {parsed:?}\n{text}"
            );
            assert_eq!(
                parsed.unrecoverable(),
                Some(Unrecoverable::Unreadable),
                "{case}: {parsed:?}\n{text}"
            );
        }
        let newer = p.replacen("\"format\":\"1.0\"", "\"format\":\"2.0\"", 1);
        for (case, text) in [
            ("newer_major", newer),
            (
                "newer_major_that_does_not_decode",
                "{\"type\":\"prepared\",\"format\":\"2.0\",\"everything\":\"else\"}\n".to_owned(),
            ),
        ] {
            let parsed = parse(&text);
            assert_eq!(parsed.format, Some(SchemaVersion::new(2, 0)), "{case}");
            assert_eq!(
                parsed.unrecoverable(),
                Some(Unrecoverable::TooNew),
                "{case}"
            );
        }
        let newer_minor = p.replacen("\"format\":\"1.0\"", "\"format\":\"1.7\"", 1);
        let parsed = parse(&format!(
            "{newer_minor}{{\"type\":\"step\",\"seq\":1,\"later\":true}}\n"
        ));
        assert_eq!(
            (parsed.phase, parsed.unrecoverable()),
            (RepairPhase::Stepped(1), None),
            "a newer minor and an unknown field read normally: {parsed:?}"
        );
    }

    #[test]
    fn a_torn_last_line_is_ignored() {
        let home = repair_home();
        let two = prepared(&home, &[flatten(&home)]);
        for (case, text, phase) in [
            (
                "cut_step",
                journal(&two, &["{\"type\":\"st".to_owned()]),
                RepairPhase::Prepared,
            ),
            (
                "cut_done",
                journal(&two, &[step(1), step(2), "{\"type\":\"do".to_owned()]),
                RepairPhase::Stepped(2),
            ),
            (
                "not_json_last",
                journal(&two, &[step(1), "garbage\n".to_owned()]),
                RepairPhase::Stepped(1),
            ),
        ] {
            let parsed = parse(&text);
            assert_eq!(
                (parsed.phase, parsed.torn_tail, parsed.unrecoverable()),
                (phase, true, None),
                "{case}: {parsed:?}\n{text}"
            );
        }
        // No prefix of a whole journal panics or reads as more than it holds.
        let whole = journal(
            &two,
            &[
                step(1),
                step(2),
                line(&RepairLine::Done),
                line(&RepairLine::Committed),
            ],
        );
        for end in (0..=whole.len()).filter(|&end| whole.is_char_boundary(end)) {
            let parsed = parse(&whole[..end]);
            assert!(parsed.steps <= 2, "{end}: {parsed:?}");
            assert_ne!(parsed.unrecoverable(), Some(Unrecoverable::TooNew), "{end}");
        }
    }

    fn touch(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "").unwrap();
    }

    #[test]
    fn list_returns_only_repair_journals() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::with_roots(
            dir.path(),
            dir.path().join("c"),
            dir.path().join("s"),
            dir.path().join("k"),
        );
        assert_eq!(list(&paths).unwrap(), Vec::new(), "no journal directory");
        let id = |n: u32| -> TxId { format!("01JB8Q2VA0000000000000000{n}").parse().unwrap() };
        let journal = paths.journal_dir();
        for name in [
            format!("{}.jsonl", id(1)),
            format!("{}.wtf.jsonl", id(2)),
            format!("{}.repair.jsonl", id(4)),
            format!("{}.repair.jsonl", id(3)),
            "x.repair.jsonl".to_owned(),
            format!("{}.reactivating.jsonl", id(5)),
            format!("{}.repair.json", id(6)),
        ] {
            touch(&journal.join(name));
        }
        std::fs::create_dir_all(journal.join(format!("{}.repair.jsonl", id(7)))).unwrap();
        let want = vec![
            (id(3), journal.join(format!("{}.repair.jsonl", id(3)))),
            (id(4), journal.join(format!("{}.repair.jsonl", id(4)))),
        ];
        assert_eq!(list(&paths).unwrap(), want);
        let folder: Vec<TxId> = crate::engine::journal::list(&paths)
            .unwrap()
            .into_iter()
            .map(|(tx, _)| tx)
            .collect();
        assert_eq!(
            folder,
            [id(1), id(2)],
            "the folder listing never returns a repair journal"
        );
        assert_eq!(
            crate::state::paths::tx_named(&format!("{}.repair", id(3))),
            None,
            "tx_named never learns the repair family"
        );
        let parsed = read(&paths, &journal.join(format!("{}.repair.jsonl", id(3)))).unwrap();
        assert_eq!(parsed.phase, RepairPhase::Unprepared);
    }

    #[test]
    fn the_history_line_is_built_from_prepared_alone() {
        let home = repair_home();
        let mut p = prepared(&home, &[flatten(&home), write(&home)]);
        p.reason = Some("nested addon".to_owned());
        let entry = history_line(&p);
        assert!(entry.well_formed());
        assert_eq!(entry.schema_version, history::CURRENT);
        assert_eq!(
            (entry.tx, entry.kind, entry.at),
            (p.tx, TxKind::Repair, p.at)
        );
        assert_eq!(entry.target, None);
        assert_eq!(entry.targets, vec![home.key]);
        assert_eq!(
            (entry.actor, entry.reason.as_deref()),
            (Actor::Human, Some("nested addon"))
        );
        assert_eq!(entry.argv, p.argv);
        assert!(entry.ops.is_empty());
        assert_eq!(entry.result, TxResult::Committed);
        assert_eq!(
            (entry.reverts, entry.backup.as_ref(), entry.plan),
            (None, None, None)
        );
        assert_eq!(entry.run, Some(p.run));
        assert_eq!(entry.doctor_run_id.as_deref(), Some(REPAIR_RUN_ID));
        assert_eq!(entry.repairs.len(), 3, "two renames and a write");
        assert_eq!(
            entry.reach(),
            Reach::Targets([home.key].into()),
            "the client's target, from the flatten's entries"
        );
        let json = serde_json::to_value(&entry).unwrap();
        assert_eq!(json["target"], Value::Null, "written as null");
        let again = serde_json::to_vec(&history_line(&p)).unwrap();
        assert_eq!(
            serde_json::to_vec(&entry).unwrap(),
            again,
            "the same bytes every time"
        );
        // Recovery's roll-forward has only the journal: the same line from its text.
        let text = journal(&p, &[step(1), step(2), step(3), line(&RepairLine::Done)]);
        let read_back = parse(&text).prepared.unwrap();
        assert_eq!(
            serde_json::to_vec(&history_line(&read_back)).unwrap(),
            again
        );
    }

    #[test]
    fn repairs_entries_equal_the_journal_primitives() {
        let home = repair_home();
        let stuck = home.stuck();
        let mut cases: Vec<(&str, Step, Option<TargetKey>, Option<&str>)> = Vec::new();
        let lowered = |fix: PlannedFix| prepared(&home, &[fix]).primitives;
        let quarantine = |path: PathBuf, root: QuarantineRoot| {
            planned("fm-schemas-state-corrupt", Fix::Quarantine { path, root })
        };
        let client = QuarantineRoot::Client {
            client_dir: home.client.client_dir().to_path_buf(),
        };
        let first = |steps: Vec<Step>| steps.into_iter().next().unwrap();
        cases.push(("write_file_over", first(lowered(write(&home))), None, None));
        cases.push((
            "write_file_creating",
            first(lowered(planned(
                SKILL,
                Fix::Write {
                    path: home.paths.skill_dir().join("NOTES.md"),
                    content: b"notes\n".to_vec(),
                    mode: Mode::new(0o644).unwrap(),
                },
            ))),
            None,
            None,
        ));
        cases.push((
            "chmod",
            first(lowered(planned(
                "fm-secrets-credential-file-mode",
                Fix::Chmod {
                    path: home.credential(),
                    mode: Mode::new(0o600).unwrap(),
                },
            ))),
            None,
            None,
        ));
        cases.push((
            "rename_of_a_dir",
            first(lowered(quarantine(home.addons("Stray"), client.clone()))),
            Some(home.key),
            Some("Stray"),
        ));
        cases.push((
            "rename_of_a_file",
            first(lowered(quarantine(home.record(), QuarantineRoot::State))),
            None,
            None,
        ));
        cases.push((
            "rename_of_a_temp_symlink",
            first(lowered(quarantine(home.temp_link(), QuarantineRoot::Cache))),
            None,
            None,
        ));
        let journal = crate::engine::repair_action::JournalSet {
            tx: stuck,
            family: crate::engine::repair_action::JournalFamily::Folder,
            target: Some(home.key),
            artifacts: vec![
                SuspendedArtifact {
                    path: home.paths.journal(&stuck),
                    root: QuarantineRoot::State,
                },
                SuspendedArtifact {
                    path: home.client.stage(&stuck),
                    root: client.clone(),
                },
            ],
            guards: vec![Guard::Generation {
                target: home.key,
                generation: 7,
            }],
            attribution: crate::engine::repair_action::Attribution::Complete,
            unattributed: Vec::new(),
        };
        let suspended = lowered(planned(
            "fm-concurrency_primitives-journal-too-new",
            Fix::Suspend { journal },
        ));
        cases.push(("with_guards", suspended[0].clone(), None, None));
        cases.push((
            "into_a_clients_meta",
            suspended[1].clone(),
            Some(home.key),
            None,
        ));
        // Undo's activation (E7), on a filesystem that records no birth time.
        let unborn = Identity {
            ino: 99,
            born_ns: None,
        };
        let file = Endpoint::File {
            identity: Some(unborn),
            sha256: None,
            mode: None,
        };
        cases.push((
            "with_activation_and_no_birth_time",
            Step {
                seq: 1,
                fix: 1,
                fm: Cow::Borrowed("fm-concurrency_primitives-journal-too-new"),
                op: Primitive::RenameNoReplace {
                    from: home.run.dir.join("quarantine/state/journal/x.jsonl"),
                    to: home.paths.journal_dir().join("x.jsonl"),
                    activation: true,
                },
                pre: Endpoints::Pair {
                    path: file.clone(),
                    to: Endpoint::Absent,
                },
                post: Endpoints::Pair {
                    path: Endpoint::Absent,
                    to: file,
                },
                guards: Vec::new(),
            },
            None,
            None,
        ));
        let dir = Endpoint::Dir {
            identity: Some(unborn),
            stamp: Some(Stamp([0x33; 32])),
        };
        cases.push((
            "flatten_into_addons",
            Step {
                seq: 1,
                fix: 1,
                fm: Cow::Borrowed(NESTED),
                op: Primitive::RenameNoReplace {
                    from: home.addons(&format!(".omaddons-flatten-{}/Same", tx())),
                    to: home.addons("Same"),
                    activation: false,
                },
                pre: Endpoints::Pair {
                    path: dir,
                    to: Endpoint::Absent,
                },
                post: Endpoints::Pair {
                    path: Endpoint::Absent,
                    to: Endpoint::Dir {
                        identity: Some(unborn),
                        stamp: None,
                    },
                },
                guards: Vec::new(),
            },
            Some(home.key),
            Some("Same"),
        ));
        let client_dirs = [ClientDirEntry {
            dir: home.client.client_dir().to_path_buf(),
            target: home.key,
        }];
        let target_of = |path: &Path| {
            client_dirs
                .iter()
                .find(|c| path.starts_with(&c.dir))
                .map(|c| c.target)
        };
        for (case, step, target, folder) in cases {
            let entry = RepairEntry::from_step(&step, target_of);
            assert_eq!(entry.target, target, "{case}: target");
            assert_eq!(
                entry.folder.as_ref().map(|f| f.as_str()),
                folder,
                "{case}: folder"
            );
            let mut got = serde_json::to_value(&entry).unwrap();
            let object = got.as_object_mut().unwrap();
            object.remove("target");
            object.remove("folder");
            let want = serde_json::to_value(&step).unwrap();
            assert_eq!(
                got, want,
                "{case}: the history entry is the journal primitive"
            );
            let back: RepairEntry = serde_json::from_value(serde_json::to_value(&entry).unwrap())
                .unwrap_or_else(|err| panic!("{case}: the entry reads back: {err}")); // ubs:ignore — fails the test
            assert_eq!(back, entry, "{case}");
        }
    }

    #[test]
    fn resources_round_trip_through_their_text() {
        let home = repair_home();
        let p = prepared(&home, &[flatten(&home), write(&home)]);
        let set = p.resources.to_set().unwrap();
        assert_eq!(ResourcesWire::of(&set).unwrap(), p.resources);
        assert!(p.resources.reads.is_empty());
        assert!(
            p.resources
                .writes
                .iter()
                .any(|text| text == &format!("backup:{REPAIR_RUN_ID}/3")),
            "{:?}",
            p.resources.writes
        );
    }
}
