//! The Omarchy bar widget (M3): its files embedded in the binary ([`assets`]), the
//! renderer that stamps one install's facts into them ([`render`]), and a port of
//! Omarchy's plugin validator ([`validate`]). `omaddons omarchy install` uses all three;
//! `status` reads an installed widget back through [`render`].

pub mod assets;
pub mod render;
pub mod validate;
