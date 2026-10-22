//! `omaddons omarchy install [--no-enable] [--section left|center|right]` (M3-D8): the bar
//! widget on the Omarchy desktop, rendered for this binary and this session's
//! directories, swapped in atomically and placed on the bar unless it is there already.
//! The work is `crate::omarchy::install`'s. `omarchy status` and `remove` come with E3.

use std::io::Write;

use clap::{Args, Subcommand};
use schemars::JsonSchema;
use serde::Serialize;

use crate::cli::Context;
use crate::error::{Result, Warning};
use crate::omarchy::install::{self, Enabled, Installed, Options, PluginInfo, Section};
use crate::output::{Outcome, Style};

/// `omarchy`'s arguments.
#[derive(Debug, Clone, Args)]
pub struct OmarchyArgs {
    #[command(subcommand)]
    pub action: OmarchyAction,
}

/// What `omarchy` does.
#[derive(Debug, Clone, Subcommand)]
pub enum OmarchyAction {
    /// Put the widget on the desktop: render it for this binary, swap it in, place it on
    /// the bar
    Install {
        /// Install the files and leave the bar as it is
        #[arg(long)]
        no_enable: bool,
        /// Where on the bar a widget not there yet goes (one already there stays put)
        #[arg(long, value_enum, default_value_t = Section::Right)]
        section: Section,
    },
}

impl OmarchyAction {
    /// The command path, as the envelope names it.
    pub const fn command(&self) -> &'static str {
        match self {
            Self::Install { .. } => "omarchy install",
        }
    }
}

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

/// Runs `omarchy <action>`.
pub fn run(ctx: &Context, args: &OmarchyArgs) -> Result<InstallOutcome> {
    match &args.action {
        OmarchyAction::Install { no_enable, section } => {
            let report = install::install(
                ctx,
                Options {
                    enable: !no_enable,
                    section: *section,
                },
            )?;
            Ok(InstallOutcome {
                data: InstallData {
                    dry_run: ctx.dry_run,
                    installed: report.installed,
                    enabled: report.enabled,
                    section: *section,
                    plugin: report.plugin,
                },
                warnings: report.warnings,
            })
        }
    }
}
