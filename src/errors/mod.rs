//! Lua errors (`omaddons errors`): BugGrabber's SavedVariables, read by [`crate::lua`],
//! decoded into one typed model whatever version and client wrote them.

pub mod buggrabber;

pub use buggrabber::{BugDb, BugEntry};
