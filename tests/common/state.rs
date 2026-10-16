//! State readers for assertions: they load through the library's own readers, so a
//! format change breaks the tests loudly instead of being parsed around.

use omaddons::model::TargetKey;
use omaddons::state::files_index::FilesIndex;
use omaddons::state::history::{self, HistoryEntry, HistoryFilter};
use omaddons::state::records::{TargetRecord, load_record};
use omaddons::state::registry::load_registry;
use omaddons::state::store;

use super::home::Home;
use super::http::paths;

/// The key of the registered target `target_id`.
pub fn key_of(home: &Home, target_id: &str) -> TargetKey {
    let registry = load_registry(&paths(&home.env)).unwrap().value;
    let ids: Vec<&str> = registry.targets.iter().map(|t| t.id.as_str()).collect();
    let found = registry.targets.iter().find(|t| t.id == target_id);
    assert!(
        found.is_some(),
        "no target {target_id}; registered: {ids:?}"
    );
    found.unwrap().key
}

/// The committed record of `target_id` (empty when nothing was installed).
pub fn record(home: &Home, target_id: &str) -> TargetRecord {
    load_record(&paths(&home.env), &key_of(home, target_id))
        .unwrap()
        .value
}

/// The files index of `target_id`.
pub fn files_index(home: &Home, target_id: &str) -> FilesIndex {
    let path = paths(&home.env).files_index(&key_of(home, target_id));
    let loaded = store::load::<FilesIndex>(&path).unwrap();
    assert!(loaded.is_some(), "no files index at {}", path.display());
    loaded.unwrap().value
}

/// Every history entry, oldest first; a torn last line is skipped.
pub fn history(home: &Home) -> Vec<HistoryEntry> {
    history::read(&paths(&home.env), &HistoryFilter::default())
        .unwrap()
        .entries
}
