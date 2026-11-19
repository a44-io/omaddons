//! The Omarchy bar widget (M3): the `omarchy` commands, [`install`], [`status`] and
//! [`remove`], which add the widget's repository through Omarchy's own plugin command
//! (omad-widget-plugin-3bcbh.9) and place it on the bar; the Omarchy tools omaddons runs
//! ([`shell`]); a port of Omarchy's plugin validator ([`validate`]); a run's data context
//! ([`context`]); and the reading of a widget an older omaddons rendered ([`legacy`]),
//! which `install` replaces.

pub mod context;
pub mod install;
pub mod legacy;
pub mod remove;
pub mod shell;
pub mod status;
pub mod validate;
