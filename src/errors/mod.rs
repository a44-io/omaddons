//! Lua errors (`omaddons errors`): BugGrabber's SavedVariables, read by [`crate::lua`],
//! decoded into one typed model whatever version and client wrote them.

pub mod attribute;
pub mod buggrabber;
pub mod frames;
pub mod locate;
pub mod sanitize;

pub use buggrabber::{BugDb, BugEntry};
