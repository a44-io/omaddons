//! omaddons: the sole authority over every World of Warcraft client on a Linux machine.
//!
//! The binary in `main.rs` parses arguments, calls into [`app`], and renders the typed
//! outcome through [`output`]. Commands never print. Every filesystem mutation goes
//! through [`fsx`].

// Release-mode unit tests skip the failpoint tests (failpoints are debug-only, DEC-7),
// leaving their imports and helpers unused there.
#![cfg_attr(all(test, not(debug_assertions)), allow(unused_imports, dead_code))]

pub mod app;
pub mod cli;
pub mod clock;
pub mod discover;
pub mod drift;
pub mod engine;
pub mod error;
pub mod fsx;
pub mod inventory;
pub mod limits;
pub mod model;
pub mod net;
pub mod output;
pub mod politeness;
pub mod resolve;
pub mod rules;
pub mod sources;
pub mod state;
pub mod text;
pub mod toc;

#[cfg(test)]
mod source_scan;
