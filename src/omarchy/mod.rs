//! The Omarchy bar widget (M3): its files embedded in the binary ([`assets`]), the
//! renderer that stamps one install's facts into them ([`render`]), a port of Omarchy's
//! plugin validator ([`validate`]), the Omarchy tools omaddons runs ([`shell`]), and the
//! `omarchy` commands: [`install`], [`status`] and [`remove`].

pub mod assets;
pub mod install;
pub mod remove;
pub mod render;
pub mod shell;
pub mod status;
pub mod validate;
