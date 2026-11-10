//! Resolution: candidate selection per target, the built-in catalog, dependencies,
//! adoption evidence, search ranking and update statuses, decided from what they are
//! given. The one exception is [`cache`]'s `ReleaseCache`, the one way a command gets
//! release metadata: it fetches from the sources and reads and writes the release memo.

pub mod adopt;
pub mod cache;
pub mod catalog;
pub mod deps;
pub mod search;
pub mod select;
pub mod updates;
