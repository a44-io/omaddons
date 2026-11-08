//! `omaddons omarchy [status|install|remove]` (M3-D8): the bar widget on the Omarchy
//! desktop. Bare `omarchy` is `omarchy status`. The work is `crate::omarchy`'s:
//! `status` reads, `install` renders the widget for this binary and this session's
//! directories, swaps it in atomically and places it on the bar unless it is there
//! already, and `remove` takes it off the bar and deletes it.

use std::io::Write;
use std::path::PathBuf;

use clap::{Args, Subcommand};
use schemars::JsonSchema;
use serde::Serialize;

use crate::cli::Context;
use crate::error::{CommandHint, Result, Warning};
use crate::omarchy::install::{self, Enabled, Installed, Options, PluginInfo, Section};
use crate::omarchy::remove;
use crate::omarchy::status::{self, OmarchyInfo, Validation};
use crate::output::{Outcome, Style};

/// `omarchy`'s arguments.
#[derive(Debug, Clone, Args)]
pub struct OmarchyArgs {
    #[command(subcommand)]
    pub action: Option<OmarchyAction>,
}

/// What `omarchy` does.
#[derive(Debug, Clone, Subcommand)]
pub enum OmarchyAction {
    /// Show whether the bar widget is installed and current
    ///
    /// Installed, current and on the bar. Bare omarchy runs this.
    Status,
    /// Install the bar widget and put it on the bar
    ///
    /// Render it for this binary, swap it in, and place it on the bar.
    Install {
        /// Install the files and leave the bar as it is
        #[arg(long)]
        no_enable: bool,
        /// Where on the bar a widget not there yet goes
        ///
        /// A widget already on the bar stays where it is.
        #[arg(long, value_enum, default_value_t = Section::Right)]
        section: Section,
    },
    /// Take the bar widget off the bar and delete it
    ///
    /// Omarchy drops its bar settings with it.
    Remove,
}

impl OmarchyAction {
    /// The command path, as the envelope names it.
    pub const fn command(&self) -> &'static str {
        match self {
            Self::Status => "omarchy status",
            Self::Install { .. } => "omarchy install",
            Self::Remove => "omarchy remove",
        }
    }
}

/// "the omaddons widget (a44.omaddons 0.1.0)".
fn widget(plugin: &PluginInfo) -> String {
    match &plugin.version {
        Some(version) => format!("the omaddons widget ({} {version})", plugin.id),
        None => format!("the omaddons widget ({})", plugin.id),
    }
}

// ---------------------------------------------------------------------------------------
// Status.

/// `omarchy status`'s data.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct StatusData {
    pub omarchy: OmarchyInfo,
    pub plugin: PluginInfo,
    /// omaddons' port of Omarchy's validator over the installed widget; `null` when
    /// none is installed.
    pub validation: Option<Validation>,
}

/// `omarchy status`'s outcome.
#[derive(Debug)]
pub struct StatusOutcome {
    data: StatusData,
    commands: Vec<CommandHint>,
}

impl Outcome for StatusOutcome {
    type Data = StatusData;

    fn command(&self) -> &'static str {
        "omarchy status"
    }

    fn data(&self) -> &StatusData {
        &self.data
    }

    fn commands(&self) -> &[CommandHint] {
        &self.commands
    }

    fn render_human(&self, w: &mut dyn Write, _style: &Style) -> std::io::Result<()> {
        let d = &self.data;
        let p = &d.plugin;
        if p.installed {
            // Installed under other HOME or XDG directories: it serves that state, and
            // nothing is recommended from here. One whose context cannot be read
            // (`Build.js` missing) serves none, and its reinstall is recommended.
            let foreign = p.context_known && !p.context_matches;
            let place = match p.section {
                Some(section) => format!("on the bar's {} section", section.as_str()),
                None if foreign => "not on the bar".to_owned(),
                None => "not on the bar (enable it: omaddons omarchy install)".to_owned(),
            };
            writeln!(
                w,
                "{} is installed in {}, {place}",
                widget(p),
                p.path.display()
            )?;
            if let Some(binary) = &p.binary {
                writeln!(w, "it runs {}", binary.display())?;
            }
            if p.outdated && foreign {
                writeln!(w, "it is outdated: this omaddons renders another widget")?;
            } else if p.outdated {
                writeln!(
                    w,
                    "it is outdated: this omaddons renders another widget (reinstall: omaddons omarchy install)"
                )?;
            } else {
                writeln!(w, "it is current")?;
            }
            if foreign {
                writeln!(
                    w,
                    "note: it serves another omaddons state (other HOME or XDG directories); installing from here would point it at this one"
                )?;
            }
            if let Some(validation) = d.validation.as_ref().filter(|v| !v.ok) {
                writeln!(w, "it is not a valid plugin:")?;
                for error in &validation.errors {
                    writeln!(w, "  {error}")?;
                }
            }
        } else {
            writeln!(
                w,
                "{} is not installed (omaddons omarchy install puts it on the bar)",
                widget(p)
            )?;
        }
        let o = &d.omarchy;
        writeln!(
            w,
            "Omarchy: {}; its shell is {}; omarchy-plugin-validate is {}",
            if o.present { "present" } else { "not found" },
            if o.shell_running {
                "running"
            } else {
                "not running"
            },
            if o.validator {
                "on PATH"
            } else {
                "not on PATH"
            }
        )
    }
}

/// `omarchy [status]`.
pub fn status() -> Result<StatusOutcome> {
    let report = status::status()?;
    Ok(StatusOutcome {
        data: StatusData {
            omarchy: report.omarchy,
            plugin: report.plugin,
            validation: report.validation,
        },
        commands: report.commands,
    })
}

// ---------------------------------------------------------------------------------------
// Install.

/// `omarchy install`'s data.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct InstallData {
    pub dry_run: bool,
    pub installed: Installed,
    pub enabled: Enabled,
    /// The section asked for (`--section`).
    pub section: Section,
    /// The widget as it is after the run (under `--dry-run`, as it is now).
    pub plugin: PluginInfo,
}

/// `omarchy install`'s outcome.
#[derive(Debug)]
pub struct InstallOutcome {
    data: InstallData,
    warnings: Vec<Warning>,
}

impl Outcome for InstallOutcome {
    type Data = InstallData;

    fn command(&self) -> &'static str {
        "omarchy install"
    }

    fn data(&self) -> &InstallData {
        &self.data
    }

    fn warnings(&self) -> &[Warning] {
        &self.warnings
    }

    fn render_human(&self, w: &mut dyn Write, _style: &Style) -> std::io::Result<()> {
        let d = &self.data;
        let what = format!(
            "the omaddons widget ({} {})",
            d.plugin.id,
            d.plugin
                .version
                .as_deref()
                .unwrap_or(env!("CARGO_PKG_VERSION"))
        );
        let where_ = d.plugin.path.display();
        match (d.dry_run, d.installed) {
            (true, Installed::Unchanged) | (false, Installed::Unchanged) => {
                writeln!(w, "{what} is installed and current in {where_}")?;
            }
            (true, _) => writeln!(w, "would install {what} in {where_}")?,
            (false, Installed::Fresh | Installed::Replaced) => {
                writeln!(w, "installed {what} in {where_}")?;
            }
        }
        let section = d.plugin.section.unwrap_or(d.section).as_str();
        match d.enabled {
            Enabled::Yes if d.dry_run => {
                writeln!(w, "would enable it in the bar's {section} section")?
            }
            Enabled::Yes => writeln!(w, "enabled it in the bar's {section} section")?,
            Enabled::Already => writeln!(w, "it is on the bar already, in the {section} section")?,
            Enabled::Skipped => writeln!(w, "left the bar as it is (--no-enable)")?,
            Enabled::Failed => writeln!(w, "could not put it on the bar (see the warning)")?,
        }
        if let Some(binary) = &d.plugin.binary {
            writeln!(
                w,
                "the widget runs {} with this session's omaddons directories",
                binary.display()
            )?;
        }
        writeln!(
            w,
            "reinstall after moving the binary or changing XDG directories: omaddons omarchy install"
        )
    }
}

/// `omarchy install`.
pub fn install(ctx: &Context, enable: bool, section: Section) -> Result<InstallOutcome> {
    let report = install::install(ctx, Options { enable, section })?;
    Ok(InstallOutcome {
        data: InstallData {
            dry_run: ctx.dry_run,
            installed: report.installed,
            enabled: report.enabled,
            section,
            plugin: report.plugin,
        },
        warnings: report.warnings,
    })
}

// ---------------------------------------------------------------------------------------
// Remove.

/// `omarchy remove`'s data.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct RemoveData {
    pub dry_run: bool,
    /// The widget's folder is gone (under `--dry-run`, would be).
    pub removed: bool,
    /// The widget is off the bar: it was on no section, or Omarchy took it off.
    pub disabled: bool,
    /// Omarchy dropped the widget's bar settings (`binary`, `panelWidth`, …) with its
    /// layout entry.
    pub settings_dropped: bool,
}

/// `omarchy remove`'s outcome.
#[derive(Debug)]
pub struct RemoveOutcome {
    data: RemoveData,
    path: PathBuf,
    warnings: Vec<Warning>,
}

impl Outcome for RemoveOutcome {
    type Data = RemoveData;

    fn command(&self) -> &'static str {
        "omarchy remove"
    }

    fn data(&self) -> &RemoveData {
        &self.data
    }

    fn warnings(&self) -> &[Warning] {
        &self.warnings
    }

    fn render_human(&self, w: &mut dyn Write, _style: &Style) -> std::io::Result<()> {
        let d = &self.data;
        let path = self.path.display();
        if d.dry_run {
            writeln!(w, "would remove the omaddons widget from {path}")?;
            if d.settings_dropped {
                writeln!(
                    w,
                    "warning: Omarchy would remove the widget's bar settings with it"
                )?;
            }
            return Ok(());
        }
        writeln!(w, "removed the omaddons widget from {path}")?;
        if !d.disabled {
            writeln!(w, "it is still in Omarchy's bar layout (see the warning)")?;
        }
        if d.settings_dropped {
            writeln!(
                w,
                "warning: the widget's bar settings were removed by Omarchy with it"
            )?;
        }
        Ok(())
    }
}

/// `omarchy remove`.
pub fn remove(ctx: &Context) -> Result<RemoveOutcome> {
    let removal = remove::remove(ctx)?;
    Ok(RemoveOutcome {
        data: RemoveData {
            dry_run: ctx.dry_run,
            removed: true,
            disabled: removal.disabled,
            settings_dropped: removal.settings_dropped,
        },
        path: removal.path,
        warnings: removal.warnings,
    })
}
