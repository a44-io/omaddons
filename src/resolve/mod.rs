//! Pure resolution: candidate selection per target, archive verification against
//! `ClientRules`, dependencies, and the built-in catalog.

pub mod adopt;
pub mod cache;
pub mod catalog;
pub mod deps;
pub mod select;
pub mod updates;
