//! `omaddons omarchy install` (M3-D6, M3-D7, M3-D8, M3-D38): renders the embedded
//! widget for this binary and this run's context, stages it in a hidden folder this run
//! owns, validates it, publishes it with one atomic rename under the global lock, records
//! the install in history, rings the doorbell and puts the widget on the bar.
//!
//! The widget's folder is `<plugins>/a44.omaddons`, the one Omarchy's `omarchy plugin
//! add` gives its id (omad-o8g4). Whatever is there is replaced as a whole, so the
//! marketplace's setup plugin, which has the same id, becomes the real widget in place.
//!
//! Each run owns `<plugins>/.a44.omaddons.stage-<op>` under an in-flight lock, taken
//! before the global lock and never waited on while holding it. A stage is reclaimed only
//! when its lock is free (its process is gone), never by age. The swap is a
//! `RENAME_EXCHANGE`, or a first install's `RENAME_NOREPLACE`; where the filesystem has
//! neither, the live widget is parked as `.a44.omaddons.old-<op>` first, so a crash
//! between the two renames leaves the widget absent until the next install, which
//! finishes. A widget an older build left in `<plugins>/omaddons` would claim the id
//! beside the new one, so the publish parks it the same way, under the same lock, before
//! the rescan.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::Serialize;
use serde_json::{Value, json};

use super::render::{self, Context as WidgetContext, Rendered};
use super::{shell, validate};
use crate::cli::Context;
use crate::engine::fence;
use crate::error::{Error, ErrorCode, Result, Suggestion, Warning, WarningCode};
use crate::fsx::{
    self, Content, FsxError, GlobalLock, InflightLock, LockUse, Op, PruneReason, RenameMode,
    ScopeTable, WriteSession, Writer, ensure_dir_all, failpoint, mutate,
};
use crate::limits::OMARCHY_SHELL_JSON_MAX_BYTES;
use crate::model::{TxId, TxKind};
use crate::state::history::{self, SettingEntry};
use crate::state::paths::Paths;

/// The bar sections a widget can be placed in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema, clap::ValueEnum, Default)]
#[serde(rename_all = "lowercase")]
pub enum Section {
    Left,
    Center,
    #[default]
    Right,
}

impl Section {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Center => "center",
            Self::Right => "right",
        }
    }
}

/// What the install did to the plugin folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Installed {
    /// There was none.
    Fresh,
    /// Another render was replaced.
    Replaced,
    /// The installed files are already these; nothing was written.
    Unchanged,
}

/// What the install did about the bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Enabled {
    /// Placed now.
    Yes,
    /// It was in a section of the bar already, and stays where it is.
    Already,
    /// `--no-enable`.
    Skipped,
    /// Omarchy's shell could not be told (`omarchy_shell_unreachable`).
    Failed,
}

/// The installed widget, as `omarchy install` reports it and `omarchy status` reads it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct PluginInfo {
    pub installed: bool,
    pub path: PathBuf,
    pub id: String,
    /// The installed manifest's `version`; `null` when none is installed.
    pub version: Option<String>,
    /// The binary its commands run; `null` when none is installed.
    pub binary: Option<PathBuf>,
    /// The installed code is not what this binary renders for the installed context.
    pub outdated: bool,
    /// It is in a section of the bar.
    pub enabled: bool,
    pub section: Option<Section>,
    /// It was installed under this run's `HOME` and XDG directories.
    pub context_matches: bool,
    /// Its `Build.js` says which directories it serves. Without that it serves no state,
    /// so a reinstall from any run retargets nothing (not on the wire).
    #[serde(skip)]
    pub context_known: bool,
}

/// What to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    /// Put the widget on the bar unless it is there already.
    pub enable: bool,
    pub section: Section,
}

/// What happened.
#[derive(Debug, Clone)]
pub struct Report {
    pub installed: Installed,
    pub enabled: Enabled,
    pub plugin: PluginInfo,
    pub warnings: Vec<Warning>,
}

/// Debug-only failpoints (DEC-7), outside the engine's list: a pause after staging and
/// validating, before the global lock (concurrency tests), the publishing rename and the
/// gap between the two renames of a swap without exchange (crash tests), and a switch
/// that plants an invalid render.
const STAGED: &str = "omarchy.staged";
const SWAP: &str = "omarchy.swap";
const PARKED: &str = "omarchy.parked";
const INVALID: &str = "omarchy.invalid";

/// Installs the widget; see the module docs.
pub fn install(ctx: &Context, opts: Options) -> Result<Report> {
    let paths = Paths::from_env()?;
    require_omarchy(&paths)?;
    let binary = widget_binary()?;
    let id = paths.omarchy_plugin_id();
    let rendered = render::render(&binary, &WidgetContext::of(&paths), &id)?;
    let plugin_dir = paths.omarchy_plugin_dir();
    // A legacy widget still has to go, so the install is never a no-op beside one.
    let same = legacy_widget(&paths).is_none() && matches_installed(&plugin_dir, &rendered);
    let placed = placed_in(&paths, &id);
    let enabled_now = |placed: Option<Section>| match (opts.enable, placed) {
        (false, _) => Enabled::Skipped,
        (true, Some(_)) => Enabled::Already,
        (true, None) => Enabled::Yes,
    };
    let unchanged = |paths: &Paths| Report {
        installed: Installed::Unchanged,
        enabled: enabled_now(placed),
        plugin: describe(paths),
        warnings: Vec::new(),
    };
    if same && (!opts.enable || placed.is_some()) {
        return Ok(unchanged(&paths));
    }
    if ctx.dry_run {
        dry_run(&paths, &rendered)?;
        let installed = if same {
            Installed::Unchanged
        } else if plugin_dir.exists() {
            Installed::Replaced
        } else {
            Installed::Fresh
        };
        return Ok(Report {
            installed,
            enabled: enabled_now(placed),
            plugin: describe(&paths),
            warnings: Vec::new(),
        });
    }

    let mut warnings = Vec::new();
    let installed = if same {
        Installed::Unchanged
    } else {
        publish(ctx, &paths, &rendered, &mut warnings)?
    };
    if installed != Installed::Unchanged {
        crate::state::doorbell::ring(&paths, ctx.command, ctx.actor);
        shell::rescan();
    }
    let placed = placed_in(&paths, &id);
    let enabled = match enabled_now(placed) {
        Enabled::Yes => match shell::put(&id, opts.section.as_str()) {
            shell::Placed::Yes => Enabled::Yes,
            shell::Placed::Unreachable(why) => {
                warnings.push(
                    Warning::new(
                        WarningCode::OmarchyShellUnreachable,
                        format!(
                            "the widget is installed, but Omarchy's shell could not put it on the bar ({why}); run it from the desktop session"
                        ),
                    )
                    .with_fix(Suggestion::command(["omaddons", "omarchy", "install"])),
                );
                Enabled::Failed
            }
        },
        other => other,
    };
    Ok(Report {
        installed,
        enabled,
        plugin: describe(&paths),
        warnings,
    })
}

/// Stages, validates and publishes `rendered`; what it did to the plugin folder.
fn publish(
    ctx: &Context,
    paths: &Paths,
    rendered: &Rendered,
    warnings: &mut Vec<Warning>,
) -> Result<Installed> {
    let op = TxId::new();
    let inflight = InflightLock::create(paths, &op)?;
    let scopes = fence::table(paths, &[], None)?;
    reclaim(paths, &scopes, &op);
    let stage = paths.omarchy_stage(&op);
    let staged = stage_and_validate(paths, &scopes, &stage, rendered);
    if let Err(err) = staged {
        prune_own(&scopes, &[stage], &inflight, paths, &op);
        return Err(err);
    }
    let failed = |err: std::io::Error| {
        Error::io(
            &stage,
            &err,
            Suggestion::command(["omaddons", "omarchy", "install"]),
        )
    };
    failpoint::hit(STAGED).map_err(failed)?;

    let guard = GlobalLock::acquire(paths, ctx.command)?;
    {
        use crate::engine::journal::{no_saved_variables, recover_under};
        ctx.notes.warn_all(&recover_under(
            &guard,
            paths,
            &no_saved_variables,
            ctx.command,
            ctx.actor,
        ));
    }
    let plugin_dir = paths.omarchy_plugin_dir();
    let legacy = legacy_widget(paths);
    // A competing install may have published since the render: the same bytes are
    // this run's work done; other bytes are replaced, this render being validated and
    // the newest request.
    if legacy.is_none() && matches_installed(&plugin_dir, rendered) {
        drop(guard);
        prune_own(&scopes, &[stage], &inflight, paths, &op);
        return Ok(Installed::Unchanged);
    }
    let mut s = WriteSession::untracked(Writer::Omarchy, &scopes, ctx.command);
    failpoint::hit(SWAP).map_err(failed)?;
    let (installed, leftover) = if plugin_dir.exists() {
        let exchange = Op::Rename {
            to: &plugin_dir,
            mode: RenameMode::Exchange,
        };
        match mutate(&mut s, &stage, exchange) {
            // The old widget is now at the stage's name.
            Ok(_) => (Installed::Replaced, stage.clone()),
            // Two renames: a crash between them leaves no widget and a hidden parked
            // one, which the next install prunes as it publishes a first install.
            Err(FsxError::Unsupported) => {
                warnings.push(two_renames());
                let old = paths.omarchy_old(&op);
                rename_onto_absent(&mut s, &plugin_dir, &old)?;
                failpoint::hit(PARKED).map_err(failed)?;
                rename_onto_absent(&mut s, &stage, &plugin_dir)?;
                (Installed::Replaced, old)
            }
            Err(err) => return Err(err.into()),
        }
    } else {
        rename_onto_absent(&mut s, &stage, &plugin_dir)?;
        (Installed::Fresh, stage.clone())
    };
    let mut leftovers = vec![leftover];
    if let Some(dir) = legacy {
        // Its own parked name: `old(&op)` may already hold the replaced widget.
        let parked = paths.omarchy_old(&TxId::new());
        rename_onto_absent(&mut s, &dir, &parked)?;
        leftovers.push(parked);
    }
    let mut entry = SettingEntry::now(
        TxKind::Widget,
        None,
        ctx.actor,
        ctx.reason.clone(),
        ctx.argv.clone(),
    );
    entry.tx = op;
    if let Some(warning) = history::record_setting(&mut s, paths, &entry) {
        warnings.push(warning);
    }
    drop(s);
    drop(guard);
    prune_own(&scopes, &leftovers, &inflight, paths, &op);
    Ok(installed)
}

/// The widget a build before omad-o8g4 installed in `<plugins>/omaddons`: that folder
/// when it is a real directory whose manifest has this widget's id. A folder there with
/// another id is another plugin's, and a symlink is a developer's own link: neither is
/// touched.
pub(super) fn legacy_widget(paths: &Paths) -> Option<PathBuf> {
    let dir = paths.omarchy_legacy_plugin_dir();
    if !std::fs::symlink_metadata(&dir).is_ok_and(|meta| meta.is_dir()) {
        return None;
    }
    let manifest = render::installed_manifest(&dir)?;
    let id = manifest.get("id")?.as_str()?;
    (id == paths.omarchy_plugin_id()).then_some(dir)
}

/// Renames `from` to `to`, which must not exist: `RENAME_NOREPLACE`, or, on a filesystem
/// without renameat2 flags, a plain rename once `to` is confirmed absent (under the
/// global lock, and every name here is this widget's own, so nothing can appear there).
pub(super) fn rename_onto_absent(s: &mut WriteSession<'_>, from: &Path, to: &Path) -> Result<()> {
    let rename = |mode| Op::Rename { to, mode };
    match mutate(s, from, rename(RenameMode::NoReplace)) {
        Ok(_) => Ok(()),
        Err(FsxError::Unsupported) if std::fs::symlink_metadata(to).is_err() => {
            mutate(s, from, rename(RenameMode::Replace))?;
            Ok(())
        }
        Err(err) => Err(err.into()),
    }
}

/// The swap took two renames (`atomic_swap_unavailable`, as for addons).
fn two_renames() -> Warning {
    Warning::new(
        WarningCode::AtomicSwapUnavailable,
        "this filesystem cannot swap folders atomically, so the widget was replaced with two renames",
    )
    .with_fix(Suggestion::text(
        "nothing to do: an install interrupted between the two renames leaves the widget absent until the next install, which finishes it",
    ))
}

/// Writes every rendered file into `stage` and validates it with omaddons' port of
/// Omarchy's validator and, when it is on `PATH`, Omarchy's own.
fn stage_and_validate(
    paths: &Paths,
    scopes: &ScopeTable,
    stage: &Path,
    rendered: &Rendered,
) -> Result<()> {
    let mut s = WriteSession::untracked(Writer::Omarchy, scopes, "omarchy install");
    ensure_dir_all(&mut s, &paths.omarchy_plugins_dir())?;
    write_tree(&mut s, stage, rendered)?;
    if failpoint::enabled(INVALID) {
        // The planted invalid render of the tests: a string schemaVersion.
        let manifest = stage.join(super::assets::MANIFEST);
        let op = Op::WriteFile {
            content: Content::Bytes(br#"{"schemaVersion": "1"}"#),
            mode: Some(0o644),
        };
        mutate(&mut s, &manifest, op)?;
    }
    check(stage)
}

/// The rendered files under `dir` (which must not exist).
fn write_tree(s: &mut WriteSession<'_>, dir: &Path, rendered: &Rendered) -> Result<()> {
    mutate(s, dir, Op::CreateDir)?;
    for (path, bytes) in &rendered.files {
        let target = dir.join(path);
        if let Some(parent) = target.parent() {
            ensure_dir_all(s, parent)?;
        }
        let op = Op::WriteFile {
            content: Content::Bytes(bytes.as_slice()),
            mode: Some(0o644),
        };
        mutate(s, &target, op)?;
    }
    Ok(())
}

/// Both validators over `dir`: their problems make an `internal` error, since the
/// files are embedded in the binary.
fn check(dir: &Path) -> Result<()> {
    let mut problems = validate::validate(dir);
    if let Some(theirs) = shell::validate(dir) {
        problems.extend(
            theirs
                .into_iter()
                .map(|p| format!("{}: {p}", shell::VALIDATOR)),
        );
    }
    if problems.is_empty() {
        return Ok(());
    }
    Err(Error::internal(format!(
        "the widget this build renders is not a valid Omarchy plugin: {}",
        problems.join("; ")
    ))
    .details(json!({ "reason": "plugin_invalid", "problems": problems })))
}

/// `--dry-run`: the render staged and validated in this run's cache scratch, nothing
/// under `<plugins>`.
fn dry_run(paths: &Paths, rendered: &Rendered) -> Result<()> {
    let tx = TxId::new();
    let lock = InflightLock::create_scratch(paths, &tx)?;
    let scopes = fence::table(paths, &[], None)?;
    let scratch = paths.cache_tmp(&tx);
    let result = (|| {
        let mut s = WriteSession::untracked(Writer::Derived, &scopes, "omarchy install");
        ensure_dir_all(&mut s, &scratch)?;
        let dir = scratch.join(paths.omarchy_plugin_name());
        write_tree(&mut s, &dir, rendered)?;
        check(&dir)
    })();
    let victims: Vec<PathBuf> = [scratch, paths.cache_inflight_lock(&tx)]
        .into_iter()
        .filter(|p| p.exists())
        .collect();
    let _ = fsx::prune(
        &scopes,
        PruneReason::PostCommitCleanup,
        &victims,
        LockUse::Own(&lock),
    );
    drop(lock);
    result
}

/// Prunes what this run leaves (its stage, or the old widget it parked) and its
/// in-flight lock file, then drops the lock. Best effort: the next install reclaims.
pub(super) fn prune_own(
    scopes: &ScopeTable,
    leftovers: &[PathBuf],
    inflight: &InflightLock,
    paths: &Paths,
    op: &TxId,
) {
    let leftovers: Vec<PathBuf> = leftovers.iter().filter(|p| p.exists()).cloned().collect();
    if !leftovers.is_empty() {
        let _ = fsx::prune(
            scopes,
            PruneReason::WidgetStage,
            &leftovers,
            LockUse::Acquire,
        );
    }
    let _ = fsx::prune(
        scopes,
        PruneReason::PostCommitCleanup,
        &[paths.inflight_lock(op)],
        LockUse::Own(inflight),
    );
}

/// Prunes the stages and parked old widgets of installs whose process is gone (their
/// in-flight lock free or missing), with their lock files. A live one is never touched,
/// whatever its age.
pub(super) fn reclaim(paths: &Paths, scopes: &ScopeTable, own: &TxId) {
    let Ok(entries) = std::fs::read_dir(paths.omarchy_plugins_dir()) else {
        return;
    };
    let mut victims = Vec::new();
    let mut locks = Vec::new();
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Some(op) = paths
            .omarchy_stage_op(&name)
            .or_else(|| paths.omarchy_old_op(&name))
        else {
            continue;
        };
        if op == *own || InflightLock::is_live(paths, &op).unwrap_or(true) {
            continue;
        }
        victims.push(entry.path());
        locks.push(paths.inflight_lock(&op));
    }
    if !victims.is_empty() {
        let _ = fsx::prune(scopes, PruneReason::WidgetStage, &victims, LockUse::Acquire);
    }
    locks.retain(|lock| lock.exists());
    if !locks.is_empty() {
        let _ = fsx::prune(
            scopes,
            PruneReason::PostCommitCleanup,
            &locks,
            LockUse::Acquire,
        );
    }
}

/// Whether the folder at `dir` holds exactly `rendered`'s files, byte for byte (so a
/// moved binary, whose `Build.js` differs, is restamped). Reads only files whose size
/// matches.
fn matches_installed(dir: &Path, rendered: &Rendered) -> bool {
    let expected: BTreeMap<&str, &[u8]> = rendered
        .files
        .iter()
        .map(|(path, bytes)| (path.as_str(), bytes.as_slice()))
        .collect();
    let mut seen = 0;
    let mut pending = vec![dir.to_path_buf()];
    while let Some(current) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&current) else {
            return false;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                return false;
            };
            if meta.is_dir() {
                pending.push(path);
                continue;
            }
            let Some(rel) = path.strip_prefix(dir).ok().and_then(Path::to_str) else {
                return false;
            };
            let Some(bytes) = expected.get(rel) else {
                return false;
            };
            if !meta.is_file() || meta.len() != bytes.len() as u64 {
                return false;
            }
            if std::fs::read(&path).ok().as_deref() != Some(*bytes) {
                return false;
            }
            seen += 1;
        }
    }
    seen == expected.len()
}

/// The section of Omarchy's bar layout that holds the widget `id`, read from
/// `shell.json` (bounded); `None` when it is in none, or the file cannot be read.
pub fn placed_in(paths: &Paths, id: &str) -> Option<Section> {
    layout_entry(paths, id).map(|(section, _)| section)
}

/// The widget's entry in Omarchy's bar layout (`{"id": …}` and its inline settings, or
/// a bare id) and its section, as [`placed_in`] reads them.
pub fn layout_entry(paths: &Paths, id: &str) -> Option<(Section, Value)> {
    let file = std::fs::File::open(paths.omarchy_shell_json()).ok()?;
    let mut bytes = Vec::new();
    file.take(OMARCHY_SHELL_JSON_MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > OMARCHY_SHELL_JSON_MAX_BYTES {
        return None;
    }
    let config: Value = serde_json::from_slice(&bytes).ok()?;
    let layout = config.get("bar")?.get("layout")?;
    [Section::Left, Section::Center, Section::Right]
        .into_iter()
        .find_map(|section| {
            let entries = layout.get(section.as_str())?.as_array()?;
            entries
                .iter()
                .find(|entry| {
                    entry.as_str() == Some(id)
                        || entry.get("id").and_then(Value::as_str) == Some(id)
                })
                .map(|entry| (section, entry.clone()))
        })
}

/// The installed widget as this run sees it. File reads only, all bounded, and no
/// subprocess: `status` reads it on every call (E4).
pub fn describe(paths: &Paths) -> PluginInfo {
    let dir = paths.omarchy_plugin_dir();
    let id = paths.omarchy_plugin_id();
    let manifest = render::installed_manifest(&dir);
    let installed = manifest.is_some();
    let context = render::installed_context(&dir);
    // What this binary renders for the installed context (M3-D7): another build or
    // other assets, never merely another context. A missing or foreign `Build.js` in an
    // installed widget is outdated.
    let outdated = installed
        && match (&context, render::installed_sha256(&dir)) {
            (Some(context), Some(sha)) => {
                render::content_sha256(context, &id).map_or(true, |own| own != sha)
            }
            _ => true,
        };
    let section = placed_in(paths, &id);
    PluginInfo {
        installed,
        version: manifest
            .as_ref()
            .and_then(|m| m.get("version"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        binary: render::installed_binary(&dir),
        outdated,
        enabled: section.is_some(),
        section,
        context_matches: installed && context == Some(WidgetContext::of(paths)),
        context_known: installed && context.is_some(),
        path: dir,
        id,
    }
}

/// Omarchy is here: its config dir, and its shell (`omarchy-shell` on `PATH`, or
/// `$OMARCHY_PATH/shell`).
pub fn present(paths: &Paths) -> bool {
    let shell_installed = shell::on_path(shell::SHELL)
        || std::env::var_os("OMARCHY_PATH")
            .is_some_and(|root| Path::new(&root).join("shell").is_dir());
    paths.omarchy_config_dir().is_dir() && shell_installed
}

/// [`present`], or `not_found` (`omarchy_missing`).
fn require_omarchy(paths: &Paths) -> Result<()> {
    if present(paths) {
        return Ok(());
    }
    Err(Error::new(
        ErrorCode::NotFound,
        "Omarchy is not installed here, so there is no bar for the widget",
        Suggestion::text(
            "this needs the Omarchy desktop (omarchy.org); everything else works without it",
        ),
    )
    .details(json!({ "reason": "omarchy_missing" })))
}

/// The running binary, canonical: what the widget's commands run, and what a fix run's
/// `undo.sh` runs.
pub(crate) fn current_binary() -> Result<PathBuf> {
    let exe = std::env::current_exe().map_err(|err| {
        Error::io(
            Path::new("/proc/self/exe"),
            &err,
            Suggestion::text("run omaddons from its installed path"),
        )
    })?;
    std::fs::canonicalize(&exe).map_err(|err| {
        Error::io(
            &exe,
            &err,
            Suggestion::text("run omaddons from its installed path"),
        )
    })
}

/// The binary the widget runs, `Build.js`'s `BINARY` (omad-vd1z): [`current_binary`],
/// unless mise installed it. mise keeps each version in `<installs>/<tool>/<version>/`
/// and links `<installs>/<tool>/latest` to the newest, so the stamp is then this same
/// file by its `latest` path. A version mise installs later is the one the widget runs,
/// and its version change reinstalls the widget (M3-D42). Never the
/// `omarchy-mise-install` wrapper on `PATH`, which asks mise for newer releases on every
/// run and, offline, waits on the network for over a minute.
pub(crate) fn widget_binary() -> Result<PathBuf> {
    let binary = current_binary()?;
    Ok(mise_latest(&binary).unwrap_or(binary))
}

/// `binary` (canonical) as `<…>/installs/<tool>/latest/<rest>`, when it is
/// `<…>/installs/<tool>/<version>/<rest>` and that `latest` path leads to this very file.
/// Structural: no environment is read, so a custom `MISE_DATA_DIR` works the same.
fn mise_latest(binary: &Path) -> Option<PathBuf> {
    for version in binary.ancestors().skip(1) {
        let tool = version.parent()?;
        let installs = tool.parent()?;
        if installs.file_name()? != "installs" {
            continue;
        }
        if version.file_name()? == "latest" {
            return None;
        }
        let rest = binary.strip_prefix(version).ok()?;
        let latest = tool.join("latest").join(rest);
        let same = std::fs::canonicalize(&latest).ok()? == binary;
        return same.then_some(latest);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(root: &Path) -> Paths {
        Paths::with_roots(
            root.join("home"),
            root.join("config"),
            root.join("state"),
            root.join("cache"),
        )
    }

    fn shell_json(paths: &Paths, bytes: &[u8]) {
        std::fs::create_dir_all(paths.omarchy_config_dir()).unwrap();
        std::fs::write(paths.omarchy_shell_json(), bytes).unwrap();
    }

    /// Both entry shapes count, in any section; anything unreadable, malformed or past
    /// the bound is "not placed".
    #[test]
    fn the_bar_layout_says_where_the_widget_is() {
        let dir = tempfile::tempdir().unwrap();
        let p = paths(dir.path());
        let id = "a44.omaddons";
        assert_eq!(placed_in(&p, id), None);
        shell_json(
            &p,
            br#"{"bar":{"layout":{"left":[{"id":"omarchy.menu"}],"center":[{"id":"a44.omaddons"}],"right":[]}}}"#,
        );
        assert_eq!(placed_in(&p, id), Some(Section::Center));
        assert_eq!(placed_in(&p, "a44.omaddons-checkpoint"), None);
        shell_json(
            &p,
            br#"{"bar":{"layout":{"right":["omarchy.clock","a44.omaddons"]}}}"#,
        );
        assert_eq!(placed_in(&p, id), Some(Section::Right));
        for refused in [
            &b"{not json"[..],
            br#"{"bar":{}}"#,
            br#"{"bar":{"layout":{"right":"a44.omaddons"}}}"#,
            br#"{"layout":{"right":["a44.omaddons"]}}"#,
        ] {
            shell_json(&p, refused);
            assert_eq!(
                placed_in(&p, id),
                None,
                "{}",
                String::from_utf8_lossy(refused)
            );
        }
        let mut big = br#"{"bar":{"layout":{"right":["a44.omaddons"]}},"pad":""#.to_vec();
        big.resize(usize::try_from(OMARCHY_SHELL_JSON_MAX_BYTES).unwrap(), b' ');
        big.extend_from_slice(br#""}"#);
        shell_json(&p, &big);
        assert_eq!(placed_in(&p, id), None);
    }

    /// Every rendered file, byte for byte, and nothing else: a changed, missing or extra
    /// file is a difference.
    #[test]
    fn the_installed_tree_matches_only_exactly() {
        let dir = tempfile::tempdir().unwrap();
        let p = paths(dir.path());
        let ctx = WidgetContext::of(&p);
        let rendered = render::render(Path::new("/opt/omaddons"), &ctx, WIDGET).unwrap();
        let plugin = dir.path().join("plugin");
        assert!(!matches_installed(&plugin, &rendered));
        for (path, bytes) in &rendered.files {
            let target = plugin.join(path);
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::write(target, bytes).unwrap();
        }
        assert!(matches_installed(&plugin, &rendered));
        let moved = render::render(Path::new("/usr/bin/omaddons"), &ctx, WIDGET).unwrap();
        assert!(
            !matches_installed(&plugin, &moved),
            "a moved binary is restamped"
        );

        let (first, bytes) = rendered.files.first().unwrap();
        let mut changed = bytes.clone();
        changed[0] ^= 1;
        std::fs::write(plugin.join(first), &changed).unwrap();
        assert!(!matches_installed(&plugin, &rendered));
        std::fs::write(plugin.join(first), bytes).unwrap();
        std::fs::write(plugin.join("extra.qml"), b"").unwrap();
        assert!(!matches_installed(&plugin, &rendered));
        std::fs::remove_file(plugin.join("extra.qml")).unwrap();
        std::fs::remove_file(plugin.join(first)).unwrap();
        assert!(!matches_installed(&plugin, &rendered));
    }

    /// A binary file at `rel` under `root`, by its canonical path.
    fn binary_at(root: &Path, rel: &str) -> PathBuf {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"\x7fELF").unwrap();
        std::fs::canonicalize(path).unwrap()
    }

    /// A mise-installed binary is stamped by mise's `latest` link to its tool (omad-vd1z),
    /// whatever folder the release unpacked it in, so the widget runs the newest version
    /// mise installs; anything else keeps its canonical path.
    #[test]
    fn a_mise_installed_binary_is_stamped_by_its_latest_link() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let tool = root.join("mise/installs/github-a44-io-omaddons");

        // Flat, and nested in the release's own folder.
        let flat = binary_at(&tool, "0.2.0/omaddons");
        let nested = binary_at(&tool, "0.2.0/omaddons-x86_64/omaddons");
        symlink("./0.2.0", tool.join("latest")).unwrap();
        assert_eq!(mise_latest(&flat), Some(tool.join("latest/omaddons")));
        assert_eq!(
            mise_latest(&nested),
            Some(tool.join("latest/omaddons-x86_64/omaddons"))
        );

        // An older version beside the newest that `latest` names is not it: a pin, or a
        // version mise has not pruned, runs as itself.
        let older = binary_at(&tool, "0.1.0/omaddons");
        assert_eq!(mise_latest(&older), None);

        // Under `latest` itself (never canonical, but still), outside any installs folder,
        // a tool without a `latest` link, or a version folder with nothing below it.
        assert_eq!(mise_latest(&tool.join("latest/omaddons")), None);
        assert_eq!(mise_latest(&binary_at(&root, "usr/bin/omaddons")), None);
        let unlinked = root.join("mise/installs/other");
        assert_eq!(mise_latest(&binary_at(&unlinked, "1.0.0/omaddons")), None);
        assert_eq!(mise_latest(&tool.join("0.2.0")), None);
        assert_eq!(mise_latest(Path::new("/")), None);
    }

    const WIDGET: &str = crate::state::paths::WIDGET_ID;
}
