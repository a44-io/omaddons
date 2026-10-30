//! Builders for integration tests (DEC-8): an isolated environment, synthetic WoW
//! trees, PE stubs, `product.db`, launcher configs, a fake `/proc`, `pause` failpoint
//! markers, bind mounts, addon folders, HTTP fixtures and release graphs, drift mutators, readers
//! for state and `net.log`, SavedVariables in both writer formats (BugGrabber's
//! among them), synthetic repair lines for history (`history`), a structured step log
//! (`tlog`), E11's differential of `journal::diagnose` against recovery
//! (`differential`), the doctor's envelope contract (`doctor`), shell checks of
//! completion scripts (`shells`), planning as a
//! command plans, with a persistent release cache and approval witnesses (`planning`),
//! previews bound by `--if-match`, their refusals and what they leave alone
//! (`binding`), the realistic home of 5,040 managed files, built offline
//! (`realistic`), release landings for every source (`landing`), and WowAce fixtures
//! over its recording (`wowace`).
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
pub mod binding;
pub mod buggrabber;
pub mod checkpoint;
pub mod differential;
pub mod doctor;
pub mod drift;
pub mod engine;
pub mod env;
pub mod exec;
pub mod graph;
pub mod guide;
pub mod history;
pub mod home;
pub mod http;
pub mod intent;
pub mod landing;
pub mod launchers;
pub mod lua;
pub mod memo;
pub mod mounts;
pub mod netlog;
pub mod pause;
pub mod pe;
pub mod planning;
pub mod plans;
pub mod privacy;
pub mod procfs;
pub mod product_db;
pub mod realistic;
pub mod shells;
pub mod state;
pub mod targets;
pub mod tlog;
pub mod trees;
pub mod tukui;
pub mod wowace;
pub mod wowi;
pub mod wtf;

#[allow(unused_imports)]
pub use intent::{IntentHome, intent_home};
