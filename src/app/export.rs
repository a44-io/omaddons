//! `omaddons export [-t T] [--write] [--overwrite]`: every client's addon set as one
//! `addons.toml` (M2 goal G-M2-5), which `sync` reproduces on another machine.
//!
//! The document comes from the records alone: no request, no write. A family section
//! holds what every live client of that family has alike (same ref, channel, pin and
//! written version), when all of them are exported; the rest goes in each client's own
//! section, and test clients and unplugged ones (registered, client dir gone) only ever
//! get their own. `version` is written for records with a release version only (with
//! the release's source `revision` when it has one: WowAce), and `sha256` only while the
//! archive is still in the blob cache. Addons from a local file
//! or directory are left out (`export_local_source`). Dependencies are written as locks
//! (`dependency = true`): in the family section when every live client of the family
//! records the same lock and something there requires it, else in their client's.
//!
//! The document goes to stdout (`omaddons export > dotfiles/addons.toml`). `--write`
//! writes `<config>/omaddons/addons.toml` instead (M2-D19), as `Writer::Config` under the
//! global lock, keeping the previous file as `addons.toml.bak`. It refuses a file edited
//! since omaddons wrote it unless `--overwrite`, and an empty export always (M2-D43).

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::Path;

use schemars::JsonSchema;
use serde::Serialize;
use serde_json::json;
use sha2::{Digest, Sha256};

use super::world::World;
use crate::cli::{Context, fix_argv};
use crate::discover::DiscoveredTarget;
use crate::error::{Error, ErrorCode, Result, Suggestion, Warning, WarningCode};
use crate::fsx::{
    Content, GlobalLock, Op, ScopeTable, WriteSession, Writer, ensure_dir_all, mutate,
};
use crate::model::{AddonReason, AddonRecord, Channel, Ref, TargetKey, VersionConfidence};
use crate::output::{Outcome, Style};
use crate::rules::{Family, TargetKind};
use crate::state::desired::{DesiredState, Entry, Section, render};
use crate::state::export_memo::ExportMemo;
use crate::state::records::load_record;

/// `export`'s arguments.
#[derive(Debug, Clone, Default, clap::Args)]
pub struct ExportArgs {
    /// Write <config>/omaddons/addons.toml instead of printing (the previous file is kept
    /// as addons.toml.bak)
    #[arg(long)]
    pub write: bool,
    /// With --write: replace a file changed since omaddons wrote it
    #[arg(long)]
    pub overwrite: bool,
}

/// `export`'s data.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ExportData {
    /// The file written; `null` without `--write`, under `--dry-run`, or when it already
    /// held this export.
    pub path: Option<String>,
    /// Where the previous file was kept; `null` when none was replaced.
    pub backup: Option<String>,
    /// The clients exported, by id.
    pub targets: Vec<String>,
    /// Entries across every section.
    pub entries: usize,
    /// The document.
    pub content: String,
}

/// `export`'s outcome.
#[derive(Debug)]
pub struct ExportOutcome {
    data: ExportData,
    warnings: Vec<Warning>,
    /// With `--write`, the line humans see instead of the document.
    summary: Option<String>,
    /// For `-v` log lines.
    pub notes: Vec<String>,
}

impl Outcome for ExportOutcome {
    type Data = ExportData;

    fn command(&self) -> &'static str {
        "export"
    }

    fn data(&self) -> &ExportData {
        &self.data
    }

    fn warnings(&self) -> &[Warning] {
        &self.warnings
    }

    fn render_human(&self, w: &mut dyn Write, _style: &Style) -> std::io::Result<()> {
        match &self.summary {
            Some(line) => writeln!(w, "{line}"),
            None => w.write_all(self.data.content.as_bytes()),
        }
    }
}

/// One exported client.
struct Exported {
    id: String,
    /// The family whose section it may share: live clients only, and only when every
    /// live client of the family is exported.
    shared: Option<Family>,
    /// Its records.
    addons: Vec<AddonRecord>,
}

/// One record as an entry, with what placing it needs.
#[derive(Debug, Clone)]
struct Item {
    entry: Entry,
    /// The ref's identity, channel, pin, written version, revision and `any_flavor`:
    /// equal items are one entry.
    key: (String, Channel, bool, Option<String>, Option<u32>, bool),
    dependency: bool,
    required_by: Vec<String>,
}

/// Runs `export`.
pub fn run(ctx: &Context, args: &ExportArgs) -> Result<ExportOutcome> {
    if args.overwrite && !args.write {
        let mut argv = fix_argv(&ctx.argv);
        argv.push("--write".to_owned());
        return Err(Error::new(
            ErrorCode::Usage,
            "--overwrite only applies to --write",
            Suggestion::command(argv),
        ));
    }
    let world = World::load(ctx)?;
    let mut warnings = world.warnings.clone();
    let mut notes = Vec::new();

    // Registered clients whose dir is gone: every one without -t, else the one -t names
    // by id or key.
    let key = ctx
        .target
        .as_deref()
        .and_then(|s| s.parse::<TargetKey>().ok());
    let missing: Vec<_> = world
        .resolved
        .missing
        .iter()
        .filter(|e| match &ctx.target {
            None => true,
            Some(selector) => e.id.eq_ignore_ascii_case(selector) || Some(e.key) == key,
        })
        .collect();
    let selected: Vec<&DiscoveredTarget> = match world.select_many(ctx) {
        Ok(selected) => selected,
        Err(_) if !missing.is_empty() => Vec::new(),
        Err(error) => return Err(error),
    };
    let records = world.records()?;
    // A family section applies to every live client of the family, so it needs them all
    // exported. A registered client whose directory is gone may be of any family (its
    // version is unknown): while one is, no section is a family's. `sync` refuses an
    // `unknown` family, so a client of none gets its own.
    let any_missing = !world.resolved.missing.is_empty();
    let shared = |d: &DiscoveredTarget| {
        let t = &d.target;
        let whole = world
            .resolved
            .targets
            .iter()
            .filter(|o| o.target.kind == TargetKind::Live && o.target.family == t.family)
            // ubs:ignore — comparing target ids, not secrets
            .all(|o| selected.iter().any(|s| s.target.id == o.target.id));
        let known = t.family != Family::Unknown;
        (t.kind == TargetKind::Live && whole && known && !any_missing).then_some(t.family)
    };
    let mut exported: Vec<Exported> = selected
        .iter()
        .map(|d| Exported {
            id: d.target.id.clone(),
            shared: shared(d),
            addons: records
                .get(&d.target.id)
                .map(|r| r.addons.values().cloned().collect())
                .unwrap_or_default(),
        })
        .collect();
    for entry in &missing {
        warnings.push(
            Warning::new(
                WarningCode::ExportMissingTarget,
                format!(
                    "{} is registered but {} is gone; exported from its records",
                    entry.id,
                    entry.client_dir.display()
                ),
            )
            .with_fix(Suggestion::command(["omaddons", "targets"])),
        );
        exported.push(Exported {
            id: entry.id.clone(),
            shared: None,
            addons: load_record(&world.paths, &entry.key)?
                .value
                .addons
                .into_values()
                .collect(),
        });
    }
    exported.sort_by(|a, b| a.id.cmp(&b.id));

    let blob_exists = |sha: &str| world.paths.blob(sha).is_file();
    let (state, local) = build(&exported, &blob_exists);
    for (id, a) in &local {
        warnings.push(Warning::new(
            WarningCode::ExportLocalSource,
            format!(
                "{id}: {} comes from {}, a path on this machine; not exported",
                a.name, a.r#ref
            ),
        ));
    }
    let entries = state
        .families
        .values()
        .chain(state.targets.values())
        .map(|s| s.entries.len())
        .sum();
    let mut data = ExportData {
        path: None,
        backup: None,
        targets: exported.iter().map(|e| e.id.clone()).collect(),
        entries,
        content: render(&state),
    };
    let summary = if args.write {
        Some(write(ctx, &world, args, &mut data, &mut notes)?)
    } else {
        None
    };
    Ok(ExportOutcome {
        data,
        warnings,
        summary,
        notes,
    })
}

/// The document for `exported` (sorted by id), and the local records left out, by
/// client id.
fn build(
    exported: &[Exported],
    blob_exists: &dyn Fn(&str) -> bool,
) -> (DesiredState, Vec<(String, AddonRecord)>) {
    let mut local = Vec::new();
    let items: Vec<Vec<Item>> = exported
        .iter()
        .map(|e| {
            e.addons
                .iter()
                .filter_map(|a| {
                    if matches!(a.r#ref, Ref::File { .. }) {
                        local.push((e.id.clone(), a.clone()));
                        return None;
                    }
                    Some(item(a, blob_exists))
                })
                .collect()
        })
        .collect();

    // Per shared family: what every member has alike, explicit and locked.
    let mut state = DesiredState::default();
    let families: BTreeSet<Family> = exported.iter().filter_map(|e| e.shared).collect();
    let mut common: BTreeMap<Family, (Vec<Item>, Vec<Item>)> = BTreeMap::new();
    for family in families {
        let members: Vec<&Vec<Item>> = exported
            .iter()
            .zip(&items)
            .filter(|(e, _)| e.shared == Some(family))
            .map(|(_, items)| items)
            .collect();
        let alike = |dependency: bool| -> Vec<Item> {
            let Some((first, rest)) = members.split_first() else {
                return Vec::new();
            };
            first
                .iter()
                .filter(|i| i.dependency == dependency)
                .filter(|i| !dependency || has_requester(i, first))
                .filter_map(|i| {
                    let mut shared = i.clone();
                    for other in rest {
                        let same = other.iter().find(|o| {
                            o.dependency == dependency
                                && o.key == i.key
                                && (!dependency || has_requester(o, other))
                        })?;
                        if same.entry.sha256 != shared.entry.sha256 {
                            shared.entry.sha256 = None;
                        }
                    }
                    Some(shared)
                })
                .collect()
        };
        let (explicit, locks) = (alike(false), alike(true));
        let section = Section {
            entries: explicit
                .iter()
                .chain(&locks)
                .map(|i| i.entry.clone())
                .collect(),
            line: 0,
        };
        if !section.entries.is_empty() {
            state.families.insert(family, section);
        }
        common.insert(family, (explicit, locks));
    }

    // Each client's own section: what its family section does not already say.
    for (e, items) in exported.iter().zip(&items) {
        let in_family = |i: &Item| {
            e.shared
                .and_then(|f| common.get(&f))
                .is_some_and(|(explicit, locks)| {
                    let shared = if i.dependency { locks } else { explicit };
                    // ubs:ignore — comparing entry keys (ref, channel, pin), not secrets
                    shared.iter().any(|s| s.key == i.key)
                })
        };
        let entries: Vec<Entry> = items
            .iter()
            .filter(|i| !in_family(i))
            .map(|i| i.entry.clone())
            .collect();
        if !entries.is_empty() {
            state
                .targets
                .insert(e.id.clone(), Section { entries, line: 0 });
        }
    }
    (state, local)
}

/// Whether something exported on the same client requires the lock `lock`.
fn has_requester(lock: &Item, items: &[Item]) -> bool {
    lock.required_by.iter().any(|r| {
        items.iter().any(|i| {
            i.entry
                .name
                .as_deref()
                .is_some_and(|n| n.eq_ignore_ascii_case(r))
        })
    })
}

/// A record as an entry: `version` for a release version only, with its source
/// `revision` when it has one, `sha256` while the archive is in the blob cache,
/// `dependency` for a lock.
fn item(a: &AddonRecord, blob_exists: &dyn Fn(&str) -> bool) -> Item {
    let version = (a.version_confidence == VersionConfidence::Release).then(|| a.version.clone());
    let revision = a.revision.filter(|_| version.is_some());
    let dependency = a.reason == AddonReason::Dependency;
    Item {
        key: (
            a.r#ref.identity(),
            a.channel,
            a.pinned,
            version.clone(),
            revision,
            a.any_flavor,
        ),
        entry: Entry {
            name: Some(a.name.clone()),
            r#ref: Some(a.r#ref.to_string()),
            channel: a.channel,
            version,
            pinned: a.pinned,
            dependency,
            sha256: a.blob_sha256.clone().filter(|sha| blob_exists(sha)),
            revision,
            any_flavor: a.any_flavor,
            line: 0,
        },
        dependency,
        required_by: a.required_by.clone(),
    }
}

/// `--write`: refuses an empty export, and a file changed since omaddons wrote it unless
/// `--overwrite`; keeps the previous file as `addons.toml.bak`, writes the new one and
/// records its hash. Identical content writes nothing. Returns the human summary.
fn write(
    ctx: &Context,
    world: &World,
    args: &ExportArgs,
    data: &mut ExportData,
    notes: &mut Vec<String>,
) -> Result<String> {
    let paths = &world.paths;
    let file = paths.addons_toml();
    let shown = file.display().to_string();
    if data.entries == 0 {
        return Err(Error::new(
            ErrorCode::DesiredStateInvalid,
            format!("nothing to export, so {shown} is left as it is"),
            Suggestion::text(
                "install or adopt addons first; an empty export never replaces addons.toml",
            ),
        )
        .details(json!({ "path": shown, "reason": "empty" })));
    }
    let guard = if ctx.dry_run {
        None
    } else {
        Some(GlobalLock::acquire(paths, "export")?)
    };
    if let Some(guard) = &guard {
        use crate::engine::journal::{no_saved_variables, recover_under};
        ctx.notes.warn_all(&recover_under(
            guard,
            paths,
            &no_saved_variables,
            ctx.command,
            ctx.actor,
        ));
    }
    let existing = match std::fs::read(&file) {
        Ok(bytes) => Some(bytes),
        // ubs:ignore — an io::ErrorKind, not a secret
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => {
            return Err(Error::io(
                &file,
                &err,
                Suggestion::text(format!("check {shown}")),
            ));
        }
    };
    let memo = ExportMemo::load(paths);
    let n = data.entries;
    let entries = if n == 1 { "entry" } else { "entries" };
    if let Some(old) = &existing {
        let ours = memo.hash_of(&file).is_some_and(|h| h == sha256_hex(old));
        if !ours && !args.overwrite {
            let mut argv = fix_argv(&ctx.argv);
            argv.push("--overwrite".to_owned());
            return Err(Error::new(
                ErrorCode::DesiredStateInvalid,
                format!(
                    "{shown} changed since omaddons last wrote it (or was not written by it); --overwrite replaces it, keeping it as addons.toml.bak"
                ),
                Suggestion::command(argv),
            )
            .details(json!({ "path": shown, "reason": "edited" })));
        }
        if ours && old.as_slice() == data.content.as_bytes() {
            drop(guard);
            return Ok(format!("{shown} is up to date ({n} {entries})"));
        }
    }
    if ctx.dry_run {
        return Ok(format!("would write {shown} ({n} {entries})"));
    }
    let backup = file.with_extension("toml.bak");
    let scopes = ScopeTable::new(paths, &[], None)?;
    let mut s = WriteSession::untracked(Writer::Config, &scopes, "addons.toml");
    ensure_dir_all(&mut s, paths.config_dir())?;
    if let Some(old) = &existing {
        write_file(&mut s, &backup, old)?;
        data.backup = Some(backup.display().to_string());
    }
    write_file(&mut s, &file, data.content.as_bytes())?;
    data.path = Some(shown.clone());
    // A memo that fails to save only makes the next --write ask for --overwrite.
    if let Err(error) = memo.record(&mut s, paths, &file, &sha256_hex(data.content.as_bytes())) {
        notes.push(format!("export memo not written: {}", error.message));
    }
    super::record_setting(ctx, &mut s, paths, crate::model::TxKind::Export, None);
    drop(guard);
    Ok(match &data.backup {
        Some(backup) => format!("wrote {shown} ({n} {entries}); the previous file is {backup}"),
        None => format!("wrote {shown} ({n} {entries})"),
    })
}

/// Writes `bytes` to `path`, atomically.
fn write_file(s: &mut WriteSession<'_>, path: &Path, bytes: &[u8]) -> Result<()> {
    let op = Op::WriteFile {
        content: Content::Bytes(bytes),
        mode: Some(0o644),
    };
    mutate(s, path, op)?;
    Ok(())
}

/// Lowercase hex SHA-256.
fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AddonState, FolderName, TxId};

    fn rec(name: &str, r: &str, version: &str) -> AddonRecord {
        AddonRecord {
            r#ref: r.parse().unwrap(),
            name: name.to_owned(),
            version: version.to_owned(),
            version_confidence: VersionConfidence::Release,
            release_id: Some(format!("{r}@{version}")),
            channel: Channel::Stable,
            pinned: false,
            reason: AddonReason::Explicit,
            required_by: Vec::new(),
            folders: vec![FolderName::new(name).unwrap()],
            state: AddonState::Ok,
            installed_at: "2026-10-01T00:00:00Z".parse().unwrap(),
            tx_id: TxId::new(),
            blob_sha256: None,
            revision: None,
            left_out: Vec::new(),
            any_flavor: false,
        }
    }

    fn client(id: &str, shared: Option<Family>, addons: Vec<AddonRecord>) -> Exported {
        Exported {
            id: id.to_owned(),
            shared,
            addons,
        }
    }

    /// `section → [name]` (sorted, as `render` writes them), family sections first.
    fn layout(state: &DesiredState) -> Vec<(String, Vec<String>)> {
        let names = |s: &Section| -> Vec<String> {
            let mut names: Vec<String> = s.entries.iter().filter_map(|e| e.name.clone()).collect();
            names.sort();
            names
        };
        state
            .families
            .iter()
            .map(|(f, s)| (format!("family.{}", f.as_str()), names(s)))
            .chain(state.targets.iter().map(|(id, s)| (id.clone(), names(s))))
            .collect()
    }

    #[test]
    fn what_every_member_has_alike_is_the_family_section() {
        let shared = Some(Family::Mainline);
        let dbm = rec("DBM", "gh:o/dbm", "1");
        let plater = rec("Plater", "gh:o/plater", "2");
        let mut newer = plater.clone();
        newer.version = "3".into();
        let exported = [
            client(
                "retail",
                shared,
                vec![dbm.clone(), plater.clone(), rec("Foo", "gh:o/foo", "1")],
            ),
            client("steam", shared, vec![dbm.clone(), newer]),
            client("ptr", None, vec![dbm]),
        ];
        let (state, _) = build(&exported, &|_| false);
        assert_eq!(
            layout(&state),
            [
                ("family.mainline".to_owned(), vec!["DBM".to_owned()]),
                ("ptr".to_owned(), vec!["DBM".to_owned()]),
                (
                    "retail".to_owned(),
                    vec!["Foo".to_owned(), "Plater".to_owned()]
                ),
                ("steam".to_owned(), vec!["Plater".to_owned()]),
            ],
            "a different version is a different entry"
        );
    }

    #[test]
    fn locks_follow_their_requesters_and_local_sources_are_left_out() {
        let shared = Some(Family::Mainline);
        let mut lib = rec("Lib", "gh:o/lib", "1");
        lib.reason = AddonReason::Dependency;
        lib.required_by = vec!["Foo".into()];
        let mut orphan = rec("Orphan", "gh:o/orphan", "1");
        orphan.reason = AddonReason::Dependency;
        orphan.required_by = vec!["Gone".into()];
        let local = rec("Mine", "file:/home/u/mine.zip", "1");
        let foo = rec("Foo", "gh:o/foo", "1");
        let exported = [
            client(
                "retail",
                shared,
                vec![foo.clone(), lib.clone(), orphan.clone(), local],
            ),
            client("steam", shared, vec![foo, lib, orphan]),
        ];
        let (state, local) = build(&exported, &|_| false);
        assert_eq!(
            layout(&state),
            [
                (
                    "family.mainline".to_owned(),
                    vec!["Foo".to_owned(), "Lib".to_owned()]
                ),
                ("retail".to_owned(), vec!["Orphan".to_owned()]),
                ("steam".to_owned(), vec!["Orphan".to_owned()]),
            ],
            "a lock nothing requires stays with its client"
        );
        let lock = &state.families[&Family::Mainline].entries[1];
        assert!(lock.dependency && lock.version.as_deref() == Some("1"));
        assert_eq!(local.len(), 1);
    }

    #[test]
    fn version_and_sha256_are_written_only_when_they_hold() {
        let mut adopted = rec("Foo", "gh:o/foo", "v1");
        adopted.version_confidence = VersionConfidence::Toc;
        adopted.blob_sha256 = Some("ab".repeat(32));
        let mut cached = rec("Bar", "gh:o/bar", "2");
        cached.blob_sha256 = Some("cd".repeat(32));
        let exported = [client("retail", None, vec![adopted, cached])];
        let cd = "cd".repeat(32);
        let (state, _) = build(&exported, &|sha| sha == cd);
        let entries = &state.targets["retail"].entries;
        let bar = entries
            .iter()
            .find(|e| e.name.as_deref() == Some("Bar"))
            .unwrap();
        let foo = entries
            .iter()
            .find(|e| e.name.as_deref() == Some("Foo"))
            .unwrap();
        assert_eq!(
            (bar.version.as_deref(), bar.sha256.as_deref()),
            (Some("2"), Some(cd.as_str()))
        );
        assert_eq!(
            (foo.version.as_deref(), foo.sha256.as_deref()),
            (None, None)
        );
    }
}
