//! `backup verify <ID>… [-t T]` (M3-D47): each backup named checked against its recorded
//! hashes, or given a first, `later` baseline when it has none, and the result recorded
//! (unless `--dry-run`). Per backup: a damaged one makes the run `partial`, still exit 0,
//! since the check itself succeeded. No global lock and no history line; an unknown id
//! refuses the whole run before anything is hashed, as `backup prune <ID>` does.

use std::collections::BTreeMap;
use std::io::Write;

use schemars::JsonSchema;
use serde::Serialize;

use super::backup::{listed, only, target_ref};
use super::outcome::TargetRef;
use super::world::World;
use crate::backup::verify::{VerifyOutcome, VerifyRequest, verify};
use crate::backup::{Baseline, Integrity};
use crate::cli::Context;
use crate::engine::fence;
use crate::error::{Error, Result, Warning};
use crate::model::BackupId;
use crate::output::{Outcome, Style};

pub const COMMAND: &str = "backup verify";

pub const HELP: &str = "\
Examples:
  omaddons backup verify 01M6Q2VA               check it against its hashes, or record them
  omaddons backup verify 01M6Q2VA 01M6Q8PN -t retail
  omaddons backup verify 01M6Q2VA --dry-run     check, record nothing

Hashes every file of a backup and compares it with the hashes it recorded when it was
captured with --verify, imported, or verified before. A backup without hashes gets them
recorded now, as a first baseline: later checks then catch any change, but this cannot
prove the capture itself was intact, and the output says so. A file that differs makes
the backup damaged, which backup restore then refuses. Nothing else changes.";

/// `backup verify`'s arguments.
#[derive(Debug, Clone, clap::Args)]
pub struct BackupVerifyArgs {
    /// The backups: ids, or prefixes of 8 or more characters
    #[arg(value_name = "ID", required = true)]
    pub ids: Vec<String>,
}

/// `backup verify`'s data.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct BackupVerifyData {
    pub dry_run: bool,
    /// Each client with a backup named, by id.
    pub targets: Vec<VerifyTarget>,
}

/// One client's backups verified.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct VerifyTarget {
    pub target: TargetRef,
    /// In the order named.
    pub backups: Vec<VerifyItem>,
}

/// One backup verified.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct VerifyItem {
    pub id: BackupId,
    /// What the check established: `verified` or `damaged`; `metadata_only` for a dry
    /// run of a backup without hashes, which establishes nothing.
    pub integrity: Integrity,
    /// Where a verified backup's hashes come from: `capture`, `import` or `later`.
    pub baseline: Option<Baseline>,
    /// Its regular files.
    pub files: u64,
    /// Of those, hashed (none when its files differ from its manifest).
    pub checked: u64,
    /// Of those, differing from a recorded hash.
    pub mismatched: u64,
    /// What it proves, in words: never "verified" without what it was verified against.
    pub note: String,
}

/// `backup verify`'s outcome.
#[derive(Debug)]
pub struct BackupVerifyOutcome {
    data: BackupVerifyData,
    warnings: Vec<Warning>,
    partial: bool,
}

impl Outcome for BackupVerifyOutcome {
    type Data = BackupVerifyData;

    fn command(&self) -> &'static str {
        COMMAND
    }

    fn data(&self) -> &BackupVerifyData {
        &self.data
    }

    fn warnings(&self) -> &[Warning] {
        &self.warnings
    }

    fn partial(&self) -> bool {
        self.partial
    }

    fn render_human(&self, w: &mut dyn Write, _style: &Style) -> std::io::Result<()> {
        for t in &self.data.targets {
            for b in &t.backups {
                let id = super::history::short_id(&b.id.to_string());
                let state = match b.integrity {
                    Integrity::Damaged => "damaged".to_owned(),
                    Integrity::Verified => match b.baseline {
                        Some(baseline) => format!("verified ({})", baseline_word(baseline)),
                        None => "verified".to_owned(),
                    },
                    Integrity::MetadataOnly => "not verified".to_owned(),
                };
                writeln!(
                    w,
                    "{}: backup {id} {state}: {}, {}",
                    t.target.id,
                    super::backup::files(b.files),
                    b.note
                )?;
            }
        }
        if self.data.dry_run {
            writeln!(w, "dry run: nothing was recorded")?;
        }
        Ok(())
    }
}

/// A baseline as `backup list` names it.
fn baseline_word(baseline: Baseline) -> &'static str {
    match baseline {
        Baseline::Capture => "capture",
        Baseline::Import => "import",
        Baseline::Later => "later",
    }
}

/// What `outcome` proves, in words. A dry run of a backup without hashes establishes
/// nothing, and says so.
fn note(outcome: &VerifyOutcome, recorded: bool) -> String {
    match outcome {
        VerifyOutcome::Verified {
            against: Baseline::Capture,
        } => "every file matched the hashes recorded when it was captured".to_owned(),
        VerifyOutcome::Verified {
            against: Baseline::Import,
        } => "every file matched the hashes checked when it was imported".to_owned(),
        VerifyOutcome::Verified {
            against: Baseline::Later,
        } => "every file matched the hashes an earlier verify recorded: nothing changed since then, which cannot prove the capture was intact".to_owned(),
        VerifyOutcome::BaselineRecorded if recorded => {
            "no hashes existed; recorded a baseline (this cannot prove the capture was intact)"
                .to_owned()
        }
        VerifyOutcome::BaselineRecorded => {
            "no hashes exist, and a dry run records none, so no baseline is established".to_owned()
        }
        VerifyOutcome::Mismatch { files, first } => {
            format!("{files} files differ from their recorded hashes; the first: {first}")
        }
        VerifyOutcome::Structure { first: Some(first) } => {
            format!("its files differ from its manifest; the first: {first}")
        }
        VerifyOutcome::Structure { first: None } => {
            "its files differ from its manifest".to_owned()
        }
    }
}

/// Runs `backup verify`: every id resolved first (an unknown one refuses the run), then
/// each backup verified in turn, its result recorded unless `--dry-run`.
pub fn run(ctx: &Context, args: &BackupVerifyArgs) -> Result<BackupVerifyOutcome> {
    let world = World::load(ctx)?;
    let mut warnings = world.warnings.clone();
    let listed = listed(ctx, &world, &mut warnings)?;
    let mut named: BTreeMap<String, Vec<BackupId>> = BTreeMap::new();
    for prefix in &args.ids {
        let (client, id) = only(&listed, prefix, ctx.target.as_deref())?;
        let ids = named.entry(client).or_default();
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    let mut targets = Vec::new();
    let mut partial = false;
    for (d, backups) in &listed {
        let Some(ids) = named.get(&d.target.id) else {
            continue;
        };
        let scopes = fence::table(
            &world.paths,
            std::slice::from_ref(&d.target.client_dir),
            None,
        )?;
        let mut items = Vec::new();
        for id in ids {
            let backup = backups
                .iter()
                .find(|b| b.id == *id)
                .ok_or_else(|| Error::internal("a resolved backup is listed"))?;
            let req = VerifyRequest {
                backup,
                record: !ctx.dry_run,
                command: COMMAND,
                actor: ctx.actor,
            };
            let done = verify(&world.paths, &scopes, &req, &ctx.progress)?;
            let established =
                done.recorded || !matches!(done.outcome, VerifyOutcome::BaselineRecorded);
            let integrity = if established {
                done.outcome.integrity()
            } else {
                Integrity::MetadataOnly
            };
            partial |= integrity == Integrity::Damaged;
            items.push(VerifyItem {
                id: *id,
                integrity,
                baseline: established.then(|| done.outcome.baseline()).flatten(),
                files: done.files,
                checked: done.checked,
                mismatched: done.mismatched,
                note: note(&done.outcome, done.recorded),
            });
        }
        targets.push(VerifyTarget {
            target: target_ref(d),
            backups: items,
        });
    }
    Ok(BackupVerifyOutcome {
        data: BackupVerifyData {
            dry_run: ctx.dry_run,
            targets,
        },
        warnings,
        partial,
    })
}
