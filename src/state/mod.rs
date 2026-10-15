//! Persistent state: XDG paths, `config.toml`, the registry, target records, the per-file
//! index, history, plans and `status.json`.

pub mod files_index;
pub mod history;
pub mod paths;
pub mod records;
pub mod status;
pub mod store;
