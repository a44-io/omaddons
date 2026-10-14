//! omaddons: the sole authority over every World of Warcraft client on a Linux machine.
//!
//! The binary in `main.rs` parses arguments, calls into [`app`], and renders the typed
//! outcome through [`output`]. Commands never print. Every filesystem mutation goes
//! through [`fsx`].

pub mod app;
pub mod cli;
pub mod discover;
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
pub mod toc;
