//! `omaddons omarchy remove` (M3-D6, M3-D38): the supported way to take the widget
//! away. Omarchy's own `omarchy plugin remove a44.omaddons` cannot, as it looks for a
//! folder named after the id.
//!
//! It takes the widget off the bar first, best effort (Omarchy then splices its layout
//! entry, inline settings included, out of `shell.json`; omaddons never writes it). Then,
//! owning an operation under an in-flight lock, it parks `<plugins>/omaddons` as the
//! hidden `<plugins>/.omaddons.old-<op>` with one rename under the global lock, so the
//! shell sees the widget go at once and never half-deleted, and records the removal in
//! history. Outside the lock it prunes the parked tree (and what dead installs left),
//! and rings the doorbell.

use std::path::PathBuf;

use serde_json::{Value, json};

use super::install;
use super::shell;
use crate::cli::Context;
use crate::engine::fence;
use crate::error::{Error, ErrorCode, Result, Suggestion, Warning, WarningCode};
use crate::fsx::{GlobalLock, InflightLock, WriteSession, Writer, failpoint};
use crate::model::{TxId, TxKind};
use crate::state::history::{self, SettingEntry};
use crate::state::paths::Paths;

/// What happened (under `--dry-run`, what would).
#[derive(Debug, Clone)]
pub struct Removal {
    /// The widget's folder.
    pub path: PathBuf,
    /// Off the bar now: it was on none of its sections, or Omarchy took it off.
    pub disabled: bool,
    /// Omarchy dropped the widget's bar settings with its layout entry.
    pub settings_dropped: bool,
    pub warnings: Vec<Warning>,
}

/// Debug-only failpoint (DEC-7): a pause after the disable, before the global lock,
/// for the two-removers test.
const REMOVING: &str = "omarchy.removing";

/// Removes the widget; see the module docs. Under `--dry-run`, what it would do.
pub fn remove(ctx: &Context) -> Result<Removal> {
    let paths = Paths::from_env()?;
    let plugin_dir = paths.omarchy_plugin_dir();
    if std::fs::symlink_metadata(&plugin_dir).is_err() {
        return Err(not_installed());
    }
    let id = paths.omarchy_plugin_id();
    // Disabling splices the whole entry out (Omarchy's `setEnabled(id, false)`), so the
    // settings go exactly when it succeeds on an entry that carries any. Reading the
    // file again afterwards would race the shell's own write.
    let entry = install::layout_entry(&paths, &id);
    let has_settings = entry
        .as_ref()
        .is_some_and(|(_, entry)| carries_settings(entry));
    if ctx.dry_run {
        return Ok(Removal {
            path: plugin_dir,
            disabled: true,
            settings_dropped: has_settings,
            warnings: Vec::new(),
        });
    }

    let mut warnings = Vec::new();
    let disabled = entry.is_none()
        || match shell::disable(&id) {
            Ok(()) => true,
            Err(why) => {
                warnings.push(off_the_bar_failed(&id, &why));
                false
            }
        };
    let op = TxId::new();
    let inflight = InflightLock::create(&paths, &op)?;
    let scopes = fence::table(&paths, &[], None)?;
    failpoint::hit(REMOVING).map_err(|err| {
        Error::io(
            &plugin_dir,
            &err,
            Suggestion::command(["omaddons", "omarchy", "remove"]),
        )
    })?;

    let guard = GlobalLock::acquire(&paths, ctx.command)?;
    {
        use crate::engine::journal::{no_saved_variables, recover_under};
        ctx.notes.warn_all(&recover_under(
            &guard,
            &paths,
            &no_saved_variables,
            ctx.command,
            ctx.actor,
        ));
    }
    // Another remove may have won while this one waited.
    if std::fs::symlink_metadata(&plugin_dir).is_err() {
        drop(guard);
        install::prune_own(&scopes, &[], &inflight, &paths, &op);
        return Err(not_installed());
    }
    let old = paths.omarchy_old(&op);
    let mut s = WriteSession::untracked(Writer::Omarchy, &scopes, ctx.command);
    install::rename_onto_absent(&mut s, &plugin_dir, &old)?;
    let mut entry = SettingEntry::now(
        TxKind::Widget,
        None,
        ctx.actor,
        ctx.reason.clone(),
        ctx.argv.clone(),
    );
    entry.tx = op;
    if let Some(warning) = history::record_setting(&mut s, &paths, &entry) {
        warnings.push(warning);
    }
    drop(s);
    drop(guard);

    install::reclaim(&paths, &scopes, &op);
    install::prune_own(&scopes, &[old], &inflight, &paths, &op);
    crate::state::doorbell::ring(&paths, ctx.command, ctx.actor);
    Ok(Removal {
        path: plugin_dir,
        disabled,
        settings_dropped: disabled && has_settings,
        warnings,
    })
}

/// `not_found`: there is no widget to remove.
fn not_installed() -> Error {
    Error::new(
        ErrorCode::NotFound,
        "the omaddons widget is not installed",
        Suggestion::command(["omaddons", "omarchy", "install"]),
    )
    .details(json!({ "reason": "widget_not_installed" }))
}

/// `omarchy_shell_unreachable` for a widget whose files are gone but whose layout entry
/// stays.
fn off_the_bar_failed(id: &str, why: &str) -> Warning {
    Warning::new(
        WarningCode::OmarchyShellUnreachable,
        format!(
            "the widget is removed, but Omarchy's shell could not take it off the bar ({why}), so its entry stays in Omarchy's bar layout"
        ),
    )
    .with_fix(Suggestion::text(format!(
        "run `omarchy plugin disable {id}` from the desktop session"
    )))
}

/// Whether `entry` (a bar layout entry) carries inline settings.
fn carries_settings(entry: &Value) -> bool {
    entry
        .as_object()
        .is_some_and(|fields| fields.keys().any(|key| key != "id"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_keys_besides_the_id_are_settings() {
        assert!(!carries_settings(&json!("a44.omaddons")));
        assert!(!carries_settings(&json!({ "id": "a44.omaddons" })));
        assert!(carries_settings(
            &json!({ "id": "a44.omaddons", "binary": "" })
        ));
    }
}
