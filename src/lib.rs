//! omaddons: the sole authority over every World of Warcraft client on a Linux machine.
//!
//! A run starts in [`cli::run`], which `main.rs` calls with the arguments: it parses
//! them, builds the run's context and dispatches into [`app`], whose commands return
//! typed outcomes that [`output`] renders. Commands never print. Every filesystem
//! mutation goes through [`fsx`].

// Release-mode unit tests skip the failpoint tests (failpoints are debug-only, DEC-7),
// leaving their imports and helpers unused there.
#![cfg_attr(all(test, not(debug_assertions)), allow(unused_imports, dead_code))]

pub mod app;
pub mod backup;
pub mod build_info;
pub mod cli;
pub mod clock;
pub mod contract;
pub mod discover;
pub mod doctor;
pub mod drift;
pub mod engine;
pub mod error;
pub mod errors;
pub mod fsx;
pub mod inventory;
pub mod limits;
pub mod lua;
pub mod model;
pub mod net;
pub mod omarchy;
pub mod output;
pub mod politeness;
pub mod privacy;
pub mod resolve;
pub mod rules;
pub mod sources;
pub mod state;
pub mod text;
pub mod toc;
pub mod vocabulary;

#[cfg(test)]
mod source_scan;
#[cfg(test)]
mod test_exec;
