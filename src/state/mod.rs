//! Persistent state: XDG paths, `config.toml`, the registry, target records, the per-file
//! index, history, plans, `status.json` and the widget's doorbell.

pub mod config;
pub mod desired;
pub mod doorbell;
pub mod export_memo;
pub mod files_index;
pub mod history;
pub mod history_tail;
pub mod memo;
pub mod paths;
pub mod records;
pub mod registry;
pub mod status;
pub mod store;
