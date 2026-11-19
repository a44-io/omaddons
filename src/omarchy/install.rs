//! `omaddons omarchy install` (M3-D6, M3-D8, M3-D38, omad-widget-plugin-3bcbh.9): puts the
//! bar widget on this desktop through Omarchy's own plugin command, from the widget's
//! repository ([`WIDGET_REPOSITORY`], or `OMADDONS_WIDGET_REPO`), and places it on the
//! bar. omaddons never writes the widget's code: `omarchy plugin add` clones the reviewed
//! repository, and `omarchy plugin update` updates it.
//!
//! What the widget's folder, `<plugins>/a44.omaddons`, holds decides the rest:
//!
//! - nothing: `omarchy-plugin-add <url> --yes` adds it (installed `fresh`);
//! - a git checkout of the widget, or a symlink (a developer's own link): nothing under
//!   it is written (`unchanged`); it is placed on the bar if it is on none;
//! - a widget an older omaddons rendered, or the setup button that installed one (a
//!   checkout of the widget's id with no service): moved out first, because Omarchy
//!   refuses an id in use, then added (`replaced`), and a `widget` history line
//!   recorded. If the add fails the older widget is moved back, and nothing is recorded;
//! - anything else: refused, and never moved.
//!
//! Moving out works as `remove` does: owning an operation under an in-flight lock, one
//! rename under the global lock to the hidden `<plugins>/.a44.omaddons.old-<op>`, which
//! Omarchy's registry ignores, pruned after the add. A widget a build before omad-o8g4
//! left in `<plugins>/omaddons` would claim the id beside the new one, so it is moved out
//! the same way.

use std::io::Read;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::Serialize;
use serde_json::{Value, json};

use super::context::Context as WidgetContext;
use super::{legacy, shell};
use crate::cli::Context;
use crate::engine::fence;
use crate::error::{Error, ErrorCode, Result, Suggestion, Warning};
use crate::fsx::{
    self, FsxError, GlobalLock, InflightLock, LockUse, Op, PruneReason, RenameMode, ScopeTable,
    WriteSession, Writer, mutate,
};
use crate::limits::OMARCHY_SHELL_JSON_MAX_BYTES;
use crate::model::{TxId, TxKind};
use crate::state::history::{self, SettingEntry};
use crate::state::paths::Paths;

/// The widget's repository, which the Omarchy plugin marketplace reviews.
pub const WIDGET_REPOSITORY: &str = "https://github.com/a44-io/omaddons-omarchy.git";
/// The variable whose git URL or local path stands for [`WIDGET_REPOSITORY`]: a local
/// export, for development builds, tests and real-machine steps. Read by its literal
/// name, as every variable is (the registry's scan).
const WIDGET_REPO_ENV: &str = "OMADDONS_WIDGET_REPO";

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
    /// There was none: Omarchy added the widget's repository.
    Fresh,
    /// A widget an older omaddons rendered, or its setup button, was moved out first.
    Replaced,
    /// The widget's checkout is there already; nothing under it was written.
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
    /// The binary a widget an older omaddons rendered runs; `null` for the widget's
    /// checkout, which finds omaddons itself, and when none is installed.
    pub binary: Option<PathBuf>,
    /// A widget an older omaddons rendered, or its setup button, which `omarchy install`
    /// replaces with the widget's checkout.
    pub outdated: bool,
    /// It is in a section of the bar.
    pub enabled: bool,
    pub section: Option<Section>,
    /// It serves this run's `HOME` and XDG directories: a widget an older omaddons
    /// rendered by what its `Build.js` records; the checkout runs in its desktop session,
    /// taken to be this run's (omad-widget-plugin-3bcbh.10 asks the session).
    pub context_matches: bool,
    /// Its files say which directories it serves: a rendered widget's `Build.js` does,
    /// a checkout's do not (not on the wire).
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

/// What the widget's folder holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Folder {
    Absent,
    /// A git checkout of the widget: a `.git` and a manifest with a service.
    Checkout,
    /// A symlink, a developer's own link: never touched.
    Link,
    /// A widget an older omaddons rendered, or the setup button (a checkout with the
    /// widget's id and no service): moved out for the checkout.
    Older,
    /// Anything else: refused.
    Foreign,
}

/// What `dir`, the widget `id`'s folder, holds.
fn folder(dir: &Path, id: &str) -> Folder {
    let Ok(meta) = std::fs::symlink_metadata(dir) else {
        return Folder::Absent;
    };
    if meta.file_type().is_symlink() {
        return Folder::Link;
    }
    if !meta.is_dir() {
        return Folder::Foreign;
    }
    let manifest = legacy::installed_manifest(dir);
    let ours = manifest
        .as_ref()
        .and_then(|m| m.get("id"))
        .and_then(Value::as_str)
        == Some(id);
    let service = manifest
        .as_ref()
        .and_then(|m| m.get("entryPoints"))
        .and_then(|e| e.get("service"))
        .is_some();
    if std::fs::symlink_metadata(dir.join(".git")).is_ok() {
        return match (ours, service) {
            (_, true) => Folder::Checkout,
            (true, false) => Folder::Older,
            (false, false) => Folder::Foreign,
        };
    }
    if ours && legacy::is_rendered(dir) {
        Folder::Older
    } else {
        Folder::Foreign
    }
}

/// Installs the widget; see the module docs.
pub fn install(ctx: &Context, opts: Options) -> Result<Report> {
    let paths = Paths::from_env()?;
    require_omarchy(&paths)?;
    let id = paths.omarchy_plugin_id();
    let plugin_dir = paths.omarchy_plugin_dir();
    let state = folder(&plugin_dir, &id);
    if state == Folder::Foreign {
        return Err(foreign(&plugin_dir));
    }
    let current = matches!(state, Folder::Checkout | Folder::Link);
    let legacy = legacy_widget(&paths);
    let placed = placed_in(&paths, &id);
    let enabled_now = |placed: Option<Section>| match (opts.enable, placed) {
        (false, _) => Enabled::Skipped,
        (true, Some(_)) => Enabled::Already,
        (true, None) => Enabled::Yes,
    };
    let installed = match state {
        Folder::Absent => Installed::Fresh,
        Folder::Older => Installed::Replaced,
        _ => Installed::Unchanged,
    };
    let url = repository()?;
    if !current && ctx.offline {
        return Err(Error::new(
            ErrorCode::Offline,
            format!("offline: not adding the widget from {url}, which omarchy plugin add clones"),
            Suggestion::text("run without --offline and with OMADDONS_OFFLINE unset"),
        )
        .details(json!({ "reason": "plugin_add_offline" })));
    }
    if ctx.dry_run || (current && legacy.is_none() && (!opts.enable || placed.is_some())) {
        return Ok(Report {
            installed,
            enabled: enabled_now(placed),
            plugin: describe(&paths),
            warnings: Vec::new(),
        });
    }

    let mut warnings = Vec::new();
    let mut older = Vec::new();
    if state == Folder::Older {
        older.push(plugin_dir.clone());
    }
    older.extend(legacy);
    if !older.is_empty() || !current {
        replace(
            ctx,
            &paths,
            &older,
            (!current).then_some(url.as_str()),
            &mut warnings,
        )?;
        crate::state::doorbell::ring(&paths, ctx.command, ctx.actor);
        if current {
            // Only the legacy folder went: Omarchy's add, which rescans, did not run.
            shell::rescan();
        }
    }
    let placed = placed_in(&paths, &id);
    let enabled = match enabled_now(placed) {
        Enabled::Yes => match shell::put(&id, opts.section.as_str()) {
            shell::Placed::Yes => Enabled::Yes,
            shell::Placed::Unreachable(why) => {
                warnings.push(
                    Warning::new(
                        crate::error::WarningCode::OmarchyShellUnreachable,
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

/// The repository to add: `OMADDONS_WIDGET_REPO` when set, else [`WIDGET_REPOSITORY`].
/// One that is empty or reads as an option is refused (`usage`) before anything runs.
fn repository() -> Result<String> {
    let Some(given) = std::env::var_os("OMADDONS_WIDGET_REPO") else {
        return Ok(WIDGET_REPOSITORY.to_owned());
    };
    match given.to_str() {
        Some(url) if !url.is_empty() && !url.starts_with('-') => Ok(url.to_owned()),
        _ => Err(Error::new(
            ErrorCode::Usage,
            format!("{WIDGET_REPO_ENV} is not a git URL or path"),
            Suggestion::text(format!(
                "set {WIDGET_REPO_ENV} to a git URL or a local path, or unset it to add {WIDGET_REPOSITORY}"
            )),
        )),
    }
}

/// Moves `older` out (one rename each under the global lock), then, when given, adds
/// the repository at `add` through Omarchy and records a `widget` history line; if the
/// add fails, the older widgets go back where they were and nothing is recorded. With
/// nothing to add (the checkout is there; only a legacy folder went), the history line
/// is recorded with the move.
fn replace(
    ctx: &Context,
    paths: &Paths,
    older: &[PathBuf],
    add: Option<&str>,
    warnings: &mut Vec<Warning>,
) -> Result<()> {
    let op = TxId::new();
    let inflight = InflightLock::create(paths, &op)?;
    let scopes = fence::table(paths, &[], None)?;
    reclaim(paths, &scopes, &op);
    let parked = match park(ctx, paths, &scopes, older, &op, add.is_none(), warnings) {
        Ok(parked) => parked,
        Err(err) => {
            prune_own(&scopes, &[], &inflight, paths, &op);
            return Err(err);
        }
    };
    if let Some(url) = add {
        let added = shell::plugin_add(url);
        // A tool that failed after moving the checkout into place (its rescan) added it.
        let id = paths.omarchy_plugin_id();
        let in_place = folder(&paths.omarchy_plugin_dir(), &id) == Folder::Checkout;
        match added {
            Err(said) if !in_place => {
                let back = unpark(ctx, paths, &scopes, &parked);
                prune_own(&scopes, &[], &inflight, paths, &op);
                back?;
                return Err(add_failed(url, &said));
            }
            Err(said) => warnings.push(Warning::new(
                crate::error::WarningCode::OmarchyShellUnreachable,
                format!("the widget was added, but Omarchy's plugin add then failed ({said})"),
            )),
            Ok(()) => {}
        }
        if let Err(err) = record(ctx, paths, &scopes, &op, warnings) {
            prune_own(&scopes, &[], &inflight, paths, &op);
            return Err(err);
        }
    }
    let leftovers: Vec<PathBuf> = parked.into_iter().map(|(_, to)| to).collect();
    prune_own(&scopes, &leftovers, &inflight, paths, &op);
    Ok(())
}

/// Renames each of `older` to a hidden `.old-<op>` name under the global lock, recording
/// the history line there too when `record_now`; `(from, to)` for each. A failed rename
/// moves back those already parked.
fn park(
    ctx: &Context,
    paths: &Paths,
    scopes: &ScopeTable,
    older: &[PathBuf],
    op: &TxId,
    record_now: bool,
    warnings: &mut Vec<Warning>,
) -> Result<Vec<(PathBuf, PathBuf)>> {
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
    let mut s = WriteSession::untracked(Writer::Omarchy, scopes, ctx.command);
    let mut parked: Vec<(PathBuf, PathBuf)> = Vec::new();
    for dir in older {
        let to = paths.omarchy_old(&TxId::new());
        if let Err(err) = rename_onto_absent(&mut s, dir, &to) {
            for (from, to) in parked.iter().rev() {
                let _ = rename_onto_absent(&mut s, to, from);
            }
            return Err(err);
        }
        parked.push((dir.clone(), to));
    }
    if record_now {
        history_line(ctx, paths, &mut s, op, warnings);
    }
    drop(s);
    drop(guard);
    Ok(parked)
}

/// Moves parked widgets back where they were, under the global lock, unless something
/// took a place since.
fn unpark(
    ctx: &Context,
    paths: &Paths,
    scopes: &ScopeTable,
    parked: &[(PathBuf, PathBuf)],
) -> Result<()> {
    let guard = GlobalLock::acquire(paths, ctx.command)?;
    let mut s = WriteSession::untracked(Writer::Omarchy, scopes, ctx.command);
    for (from, to) in parked.iter().rev() {
        if std::fs::symlink_metadata(from).is_err() {
            rename_onto_absent(&mut s, to, from)?;
        }
    }
    drop(s);
    drop(guard);
    Ok(())
}

/// The `widget` history line, under the global lock.
fn record(
    ctx: &Context,
    paths: &Paths,
    scopes: &ScopeTable,
    op: &TxId,
    warnings: &mut Vec<Warning>,
) -> Result<()> {
    let guard = GlobalLock::acquire(paths, ctx.command)?;
    let mut s = WriteSession::untracked(Writer::Omarchy, scopes, ctx.command);
    history_line(ctx, paths, &mut s, op, warnings);
    drop(s);
    drop(guard);
    Ok(())
}

fn history_line(
    ctx: &Context,
    paths: &Paths,
    s: &mut WriteSession<'_>,
    op: &TxId,
    warnings: &mut Vec<Warning>,
) {
    let mut entry = SettingEntry::now(
        TxKind::Widget,
        None,
        ctx.actor,
        ctx.reason.clone(),
        ctx.argv.clone(),
    );
    entry.tx = *op;
    if let Some(warning) = history::record_setting(s, paths, &entry) {
        warnings.push(warning);
    }
}

/// `omarchy plugin add` failed, and the older widget, if any, is back.
fn add_failed(url: &str, said: &str) -> Error {
    Error::new(
        ErrorCode::DownloadFailed,
        format!("Omarchy could not add the widget from {url}: {said}"),
        Suggestion::command(["omaddons", "omarchy", "install"]),
    )
    .details(json!({ "reason": "plugin_add_failed", "url": url, "said": said }))
}

/// The widget's folder holds something omaddons did not install.
fn foreign(dir: &Path) -> Error {
    Error::new(
        ErrorCode::FileExists,
        format!(
            "{} holds a plugin that is neither the widget's checkout nor one omaddons rendered, so it is left alone",
            dir.display()
        ),
        Suggestion::text(
            "remove that plugin with Omarchy's plugin remove, or move the folder aside, then install again",
        ),
    )
    .details(json!({ "reason": "plugin_folder_foreign", "file": dir }))
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
    let manifest = legacy::installed_manifest(&dir)?;
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

/// Prunes what this run leaves (the widgets it moved out) and its in-flight lock file,
/// then drops the lock. Best effort: the next install reclaims.
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

/// Prunes the stages (an older omaddons rendered into them) and moved-out widgets of
/// runs whose process is gone (their in-flight lock free or missing), with their lock
/// files. A live one is never touched, whatever its age.
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
/// subprocess: `status` reads it on every call (E4). A widget an older omaddons rendered
/// is outdated (`omarchy install` replaces it), and its `Build.js` says which binary it
/// runs and whose directories it serves. The widget's checkout finds omaddons itself in
/// its desktop session, so its files say neither (omad-widget-plugin-3bcbh.10 gives
/// `status` the rest).
pub fn describe(paths: &Paths) -> PluginInfo {
    let dir = paths.omarchy_plugin_dir();
    let id = paths.omarchy_plugin_id();
    let manifest = legacy::installed_manifest(&dir);
    let installed = manifest.is_some();
    let context = legacy::installed_context(&dir);
    let section = placed_in(paths, &id);
    PluginInfo {
        installed,
        version: manifest
            .as_ref()
            .and_then(|m| m.get("version"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        binary: legacy::installed_binary(&dir),
        outdated: installed && folder(&dir, &id) == Folder::Older,
        enabled: section.is_some(),
        section,
        context_matches: installed
            && (!legacy::is_rendered(&dir) || context == Some(WidgetContext::of(paths))),
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

    /// What the folder holds: a checkout of the widget, the setup button, a render, a
    /// link, nothing, or another plugin's files.
    #[test]
    fn the_folder_says_what_install_does() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("a44.omaddons");
        let id = "a44.omaddons";
        let manifest = |text: &str| {
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("manifest.json"), text).unwrap();
        };
        assert_eq!(folder(&dir, id), Folder::Absent);
        // The widget's checkout: a service, a .git.
        manifest(
            r#"{"id":"a44.omaddons","entryPoints":{"service":"Service.qml","barWidget":"BarWidget.qml"}}"#,
        );
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        assert_eq!(folder(&dir, id), Folder::Checkout);
        // The setup button: a checkout of the same id with a bar widget only.
        manifest(r#"{"id":"a44.omaddons","entryPoints":{"barWidget":"BarWidget.qml"}}"#);
        assert_eq!(folder(&dir, id), Folder::Older);
        // Another plugin's checkout with no service, in this folder: never moved.
        manifest(r#"{"id":"someone.else","entryPoints":{"barWidget":"BarWidget.qml"}}"#);
        assert_eq!(folder(&dir, id), Folder::Foreign);
        // Files copied by hand, no .git and no Build.js: never moved either.
        std::fs::remove_dir_all(dir.join(".git")).unwrap();
        manifest(r#"{"id":"a44.omaddons","entryPoints":{"service":"Service.qml"}}"#);
        assert_eq!(folder(&dir, id), Folder::Foreign);
        // A render: its service in a code folder beside a Build.js.
        manifest(
            r#"{"id":"a44.omaddons","entryPoints":{"service":"r-0123456789abcdef/Service.qml"}}"#,
        );
        std::fs::create_dir_all(dir.join("r-0123456789abcdef")).unwrap();
        std::fs::write(
            dir.join("r-0123456789abcdef/Build.js"),
            "var BINARY = \"/o\"\n",
        )
        .unwrap();
        assert_eq!(folder(&dir, id), Folder::Older);
        // A symlink is a developer's own link, whatever it points at.
        let link = root.path().join("linked");
        std::os::unix::fs::symlink(&dir, &link).unwrap();
        assert_eq!(folder(&link, id), Folder::Link);
        // A plain file is no plugin.
        let file = root.path().join("file");
        std::fs::write(&file, "x").unwrap();
        assert_eq!(folder(&file, id), Folder::Foreign);
    }
}
