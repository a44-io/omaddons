//! Builders for integration tests (DEC-8): an isolated environment, synthetic WoW
//! trees, PE stubs, `product.db`, launcher configs, a fake `/proc`, `pause` failpoint
//! markers, bind mounts, addon folders, HTTP fixtures and release graphs, drift mutators, readers
//! for state and `net.log`, and SavedVariables in both writer formats (BugGrabber's
//! among them).
//! Each integration test crate declares `mod common;` and uses the subset it needs.
//!
//! Everything is written under a [`env::TestEnv`] tempdir; nothing here touches the real
//! home directory or the test process's environment (tests run on parallel threads).
//! Builders are deterministic: the same calls produce byte-identical trees.

#![allow(dead_code)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

pub mod addons;
pub mod archives;
pub mod backups;
pub mod buggrabber;
pub mod checkpoint;
pub mod drift;
pub mod engine;
pub mod env;
pub mod graph;
pub mod home;
pub mod http;
pub mod launchers;
pub mod lua;
pub mod memo;
pub mod mounts;
pub mod netlog;
pub mod pause;
pub mod pe;
pub mod privacy;
pub mod procfs;
pub mod product_db;
pub mod state;
pub mod targets;
pub mod trees;
pub mod tukui;
pub mod wowi;
pub mod wtf;
