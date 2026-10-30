//! The detectors, one file per subsystem, so the catalog leaf that later owns a row
//! rewrites its detector in place. Each reads only the view and sets `confidence: high`.

pub(crate) mod concurrency;
pub(crate) mod external;
pub(crate) mod plugins;
pub(crate) mod schemas;
pub(crate) mod state_files;
pub(crate) mod userland;
