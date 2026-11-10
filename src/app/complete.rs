//! `omaddons __complete <KIND> [--target <TARGET>] [--prefix <TEXT>]`: the values a shell
//! completion script offers where clap's static script can only offer file names: client
//! ids, addon names, and plan, backup, transaction, run, doctor run and failure-mode ids,
//! one per line.
//!
//! It is caught in `cli::run_with` before the command line is parsed, by its first
//! argument alone, and has its own small parser: it is no subcommand of `Cli`, so no
//! help page, `capabilities`, the guide, the contract, the widget's `CLI_PATHS` or the
//! generated scripts' command lists name it, and no `Context` is built for it.
//!
//! It reads only omaddons' own small files: `registry.json` (bounded), the registered
//! targets' records, the plan store's listing, the backup inventory, the history
//! projection, the doctor's run directories' listing, and the compiled failure-mode table.
//! It never runs discovery, takes no lock, recovers nothing, makes no request and writes
//! nothing. Whatever goes wrong (an argument it cannot parse, no usable `HOME`, a file that
//! is missing, damaged or written by a newer omaddons) it offers nothing, and still exits
//! 0: a completion never puts an error under the prompt.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::io::Write;

use clap::Parser;
use schemars::JsonSchema;
use serde::Serialize;

use crate::backup::inventory;
use crate::doctor::budget::Budget;
use crate::doctor::registry::FAILURE_MODES;
use crate::doctor::runs;
use crate::limits::{COMPLETION_MAX_VALUES, DOCTOR_SCAN_MAX_ENTRIES};
use crate::output::{Outcome, Style};
use crate::state::history::line_ids;
use crate::state::history_tail;
use crate::state::paths::Paths;
use crate::state::plans;
use crate::state::records::load_record;
use crate::state::registry::{Registry, RegistryEntry};
use crate::state::store::{self, VersionStatus};

/// The first argument that selects this reader, exactly.
pub const WORD: &str = "__complete";

/// The value `--since` and `doctor diff` take for the newest doctor run.
const LATEST_RUN: &str = "latest";

/// What a completion offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, clap::ValueEnum)]
pub enum Kind {
    /// Registered client ids, sorted.
    Target,
    /// Installed addon names: one client's (`--target`), else every registered client's.
    Addon,
    /// Stored plan ids, newest first.
    Plan,
    /// `WTF/` backup ids: one client's (`--target`), else every client's, newest first.
    Backup,
    /// Transactions undo can revert, newest first, from the history projection.
    Tx,
    /// Run ids, newest first, from the history projection.
    Run,
    /// `latest`, then the doctor's published run ids, newest first.
    DoctorRun,
    /// Every failure-mode id; after a comma, the list so far is kept (`--only a,b`).
    Fm,
}

impl Kind {
    /// Every kind, in the order the shell glue declares them.
    pub const ALL: [Self; 8] = [
        Self::Target,
        Self::Addon,
        Self::Plan,
        Self::Backup,
        Self::Tx,
        Self::Run,
        Self::DoctorRun,
        Self::Fm,
    ];

    /// The word a script passes for it.
    pub const fn word(self) -> &'static str {
        match self {
            Self::Target => "target",
            Self::Addon => "addon",
            Self::Plan => "plan",
            Self::Backup => "backup",
            Self::Tx => "tx",
            Self::Run => "run",
            Self::DoctorRun => "doctor-run",
            Self::Fm => "fm",
        }
    }
}

/// `__complete`'s arguments, after the word itself.
#[derive(Debug, Parser)]
#[command(
    name = "omaddons __complete",
    no_binary_name = true,
    disable_help_flag = true,
    disable_version_flag = true
)]
struct Args {
    /// What to offer.
    #[arg(value_enum)]
    kind: Kind,
    /// The client whose addons or backups to offer: a registered id or key. Any other
    /// word (a game mode, a path, `all`) offers every registered client's.
    #[arg(long, allow_hyphen_values = true)]
    target: Option<String>,
    /// Only values starting with it, ignoring ASCII case.
    #[arg(long, allow_hyphen_values = true)]
    prefix: Option<String>,
}

/// The `data` of a `__complete` outcome. Never an envelope a user sees: the reader
/// always renders as bare lines.
#[derive(Debug, Default, Serialize, JsonSchema)]
pub struct CompleteData {
    pub values: Vec<String>,
}

/// What `__complete` returns.
#[derive(Debug, Default)]
pub struct CompleteOutcome {
    data: CompleteData,
}

impl CompleteOutcome {
    pub fn values(&self) -> &[String] {
        &self.data.values
    }
}

impl Outcome for CompleteOutcome {
    type Data = CompleteData;

    fn command(&self) -> &'static str {
        WORD
    }

    fn data(&self) -> &CompleteData {
        &self.data
    }

    /// One value per line, nothing else.
    fn render_human(&self, w: &mut dyn Write, _style: &Style) -> std::io::Result<()> {
        for value in &self.data.values {
            writeln!(w, "{value}")?;
        }
        Ok(())
    }
}

/// Whether `raw` (the process's argv) asks for this reader: its first argument is
/// [`WORD`], exactly.
pub fn is_invocation(raw: &[OsString]) -> bool {
    raw.get(1).is_some_and(|word| word == WORD)
}

/// The values for `args` (the arguments after [`WORD`]) from the files under `paths`;
/// nothing when the arguments do not parse or there are no paths.
pub fn run(args: &[OsString], paths: Option<&Paths>) -> CompleteOutcome {
    let values = Args::try_parse_from(args)
        .ok()
        .zip(paths)
        .map(|(args, paths)| values(paths, &args))
        .unwrap_or_default();
    CompleteOutcome {
        data: CompleteData { values },
    }
}

/// The candidates of `args.kind` that start with the prefix, each once, at most
/// [`COMPLETION_MAX_VALUES`].
fn values(paths: &Paths, args: &Args) -> Vec<String> {
    let prefix = args.prefix.as_deref().unwrap_or_default();
    // A comma-separated list (`--only a,b`) completes its last item, the rest kept.
    let (kept, typed) = match (args.kind, prefix.rfind(',')) {
        (Kind::Fm, Some(at)) => prefix.split_at(at + 1),
        _ => ("", prefix),
    };
    let target = args.target.as_deref();
    let candidates = match args.kind {
        Kind::Target => targets(paths),
        Kind::Addon => addons(paths, target),
        Kind::Plan => plan_ids(paths),
        Kind::Backup => backup_ids(paths, target),
        Kind::Tx => history_ids(paths, Kind::Tx),
        Kind::Run => history_ids(paths, Kind::Run),
        Kind::DoctorRun => doctor_runs(paths),
        Kind::Fm => FAILURE_MODES.iter().map(|fm| fm.id.to_owned()).collect(),
    };
    let mut seen = BTreeSet::new();
    candidates
        .into_iter()
        .filter(|value| offerable(value) && starts_with_ignoring_case(value, typed))
        .filter(|value| seen.insert(value.to_ascii_lowercase()))
        .take(COMPLETION_MAX_VALUES)
        .map(|value| format!("{kept}{value}"))
        .collect()
}

/// A value a shell can take as one word of one line: not empty, no control character.
fn offerable(value: &str) -> bool {
    !value.is_empty() && !value.chars().any(char::is_control)
}

fn starts_with_ignoring_case(value: &str, prefix: &str) -> bool {
    value
        .as_bytes()
        .get(..prefix.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(prefix.as_bytes()))
}

/// The registry's targets, read bounded as the vocabulary reads it; none when it is
/// missing, unreadable, oversized, undecodable or from a newer omaddons.
fn registry(paths: &Paths) -> Vec<RegistryEntry> {
    let path = paths.registry_file();
    crate::discover::read_bytes(&path)
        .and_then(|bytes| store::load_bytes::<Registry>(&path, &bytes).ok())
        .filter(|loaded| loaded.status != VersionStatus::Newer)
        .map(|loaded| loaded.value.targets)
        .unwrap_or_default()
}

/// The registered targets `target` names (an id or a key, ignoring ASCII case); every
/// one when it names none of them, or is absent.
fn selected(entries: Vec<RegistryEntry>, target: Option<&str>) -> Vec<RegistryEntry> {
    let names = |entry: &RegistryEntry, word: &str| {
        entry.id.eq_ignore_ascii_case(word) || entry.key.to_string().eq_ignore_ascii_case(word)
    };
    match target {
        Some(word) if entries.iter().any(|entry| names(entry, word)) => entries
            .into_iter()
            .filter(|entry| names(entry, word))
            .collect(),
        _ => entries,
    }
}

fn targets(paths: &Paths) -> Vec<String> {
    let mut ids: Vec<String> = registry(paths).into_iter().map(|entry| entry.id).collect();
    ids.sort_by_key(|id| id.to_ascii_lowercase());
    ids
}

/// The addon names the selected targets' records hold, sorted ignoring case. A record
/// that cannot be read, or that a newer omaddons wrote, adds none.
fn addons(paths: &Paths, target: Option<&str>) -> Vec<String> {
    let mut names: Vec<String> = selected(registry(paths), target)
        .iter()
        .filter_map(|entry| load_record(paths, &entry.key).ok())
        .filter(|loaded| loaded.status != VersionStatus::Newer)
        .flat_map(|loaded| loaded.value.addons.into_values().map(|addon| addon.name))
        .collect();
    names.sort_by_key(|name| name.to_ascii_lowercase());
    names
}

/// The stored plans' ids, newest first.
fn plan_ids(paths: &Paths) -> Vec<String> {
    let mut ids: Vec<String> = plans::list(paths)
        .map(|listing| {
            listing
                .plans
                .into_iter()
                .map(|plan| plan.id.to_string())
                .collect()
        })
        .unwrap_or_default();
    newest_first(&mut ids);
    ids
}

/// The selected targets' backup ids as the backup inventory last recorded them, newest
/// first.
fn backup_ids(paths: &Paths, target: Option<&str>) -> Vec<String> {
    let file = inventory::load_file(paths);
    let mut ids: Vec<String> = selected(registry(paths), target)
        .iter()
        .filter_map(|entry| file.targets.get(&entry.key))
        .flat_map(|inventory| inventory.covers.backups.iter().map(ToString::to_string))
        .collect();
    newest_first(&mut ids);
    ids
}

/// From the history projection while it is current: the transactions undo can revert
/// (`tx`), or every run (`run`), newest first.
fn history_ids(paths: &Paths, kind: Kind) -> Vec<String> {
    let Some(tail) = history_tail::fresh(paths) else {
        return Vec::new();
    };
    let mut ids: Vec<String> = tail
        .lines
        .iter()
        .filter_map(|line| line_ids(line))
        .filter_map(|ids| match kind {
            Kind::Tx => ids.tx.filter(|_| ids.undoable).map(|tx| tx.to_string()),
            _ => ids.run.map(|run| run.to_string()),
        })
        .collect();
    newest_first(&mut ids);
    ids
}

/// `latest`, then every published doctor run, newest first.
fn doctor_runs(paths: &Paths) -> Vec<String> {
    let mut budget = Budget::entries_only(DOCTOR_SCAN_MAX_ENTRIES);
    let mut ids: Vec<String> = runs::list_runs(paths, &mut budget)
        .map(|listing| {
            listing
                .runs
                .into_iter()
                .filter(|run| !run.staged)
                .map(|run| run.id.as_str().to_owned())
                .collect()
        })
        .unwrap_or_default();
    if ids.is_empty() {
        return ids;
    }
    newest_first(&mut ids);
    ids.insert(0, LATEST_RUN.to_owned());
    ids
}

/// Sorted newest first: every id here starts with its time (a ULID, or a doctor run's
/// timestamp), so that is descending order.
fn newest_first(ids: &mut [String]) {
    ids.sort_unstable_by(|a, b| b.cmp(a));
}

#[cfg(test)]
mod tests;
