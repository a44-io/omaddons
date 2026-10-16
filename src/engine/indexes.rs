//! The Tukui list and the WoWInterface index as dependency resolution reads them, after
//! the catalog misses (the resolver's steps 3 and 4). Each loads at most once per
//! command, shared by every target and round, and only when a lookup reaches it: a
//! dependency the catalog resolves never touches either.
//!
//! Tukui's list comes through the release cache (the memo inside 15 minutes, else one
//! `tukui-list` request); WoWInterface's index from its stored file while it is fresh,
//! else one `wowi-filelist` request. `--offline` reads what is stored, whatever its age,
//! and never downloads. A disabled source, or one with nothing to read, is skipped and
//! remembered, so a dependency no source ships can say which were not consulted. No
//! dependency name is ever sent to a service: both lookups are local.

use std::sync::{Mutex, OnceLock, PoisonError};

use jiff::Timestamp;

use super::plan::PlanCtx;
use crate::error::{ErrorCode, Warning};
use crate::model::FolderName;
use crate::resolve::cache::Freshness;
use crate::resolve::deps::{TukuiDirs, TukuiHit, WowiDirs, WowiProject};
use crate::rules::{ClientVersion, Family, InstallKind};
use crate::sources::SourceId;
use crate::sources::wowi::index::{IndexPolicy, WowiIndex};
use crate::state::memo::TukuiList;

/// What the indexes loaded, once per command.
#[derive(Debug, Default)]
pub struct DepIndexes {
    tukui: OnceLock<Option<TukuiList>>,
    wowi: OnceLock<Option<WowiIndex>>,
    /// The loads' warnings (`stale_check`), handed out once.
    warnings: Mutex<Vec<Warning>>,
    /// Why a source was not consulted, in lookup order.
    skipped: Mutex<Vec<(SourceId, &'static str)>>,
}

impl DepIndexes {
    /// The warnings the loads raised since the last call.
    pub fn take_warnings(&self) -> Vec<Warning> {
        std::mem::take(&mut *self.warnings.lock().unwrap_or_else(PoisonError::into_inner))
    }

    /// `Tukui (disabled); WoWInterface (offline, and no index is stored)`: the sources a
    /// lookup could not consult, in lookup order, when any.
    pub fn skipped_note(&self) -> Option<String> {
        let skipped = self.skipped.lock().unwrap_or_else(PoisonError::into_inner);
        let parts: Vec<String> = skipped
            .iter()
            .map(|(source, why)| {
                let name = match source {
                    SourceId::Tukui => "Tukui",
                    SourceId::Wowi => "WoWInterface",
                    other => other.wire(),
                };
                format!("{name} ({why})")
            })
            .collect();
        (!parts.is_empty()).then(|| parts.join("; "))
    }

    fn skip(&self, source: SourceId, why: &'static str) {
        let mut skipped = self.skipped.lock().unwrap_or_else(PoisonError::into_inner);
        if !skipped.iter().any(|(s, _)| *s == source) {
            skipped.push((source, why));
        }
    }

    fn warn(&self, warnings: Vec<Warning>) {
        self.warnings
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend(warnings);
    }
}

/// The lookups over `indexes` for one plan.
pub struct IndexLookups<'c, 'a> {
    pub indexes: &'c DepIndexes,
    pub ctx: &'c PlanCtx<'a>,
}

impl IndexLookups<'_, '_> {
    fn tukui_list(&self) -> Option<&TukuiList> {
        self.indexes
            .tukui
            .get_or_init(|| {
                if self.ctx.registry.tukui().is_none() {
                    self.indexes.skip(SourceId::Tukui, "disabled");
                    return None;
                }
                match self.ctx.cache.tukui_list() {
                    Ok(list) => Some(list),
                    Err(e) if matches!(e.code, ErrorCode::Offline) => {
                        self.indexes
                            .skip(SourceId::Tukui, "offline, and no list is remembered");
                        None
                    }
                    Err(_) => {
                        self.indexes
                            .skip(SourceId::Tukui, "its list could not be fetched");
                        None
                    }
                }
            })
            .as_ref()
    }

    fn wowi_index(&self) -> Option<&WowiIndex> {
        self.indexes
            .wowi
            .get_or_init(|| {
                if self.ctx.registry.get(SourceId::Wowi).is_none() {
                    self.indexes.skip(SourceId::Wowi, "disabled");
                    return None;
                }
                let policy = match self.ctx.cache.freshness() {
                    Freshness::OfflineOnly => IndexPolicy::Offline,
                    Freshness::Refresh => IndexPolicy::Refresh,
                    Freshness::Window(_) | Freshness::Revalidate => IndexPolicy::Auto,
                };
                match WowiIndex::load_or_refresh(self.ctx.paths, self.ctx.http, policy) {
                    Ok(load) => {
                        self.indexes.warn(load.warnings);
                        if load.index.is_none() {
                            self.indexes.skip(
                                SourceId::Wowi,
                                if policy == IndexPolicy::Offline {
                                    "offline, and no index is stored"
                                } else {
                                    "its file list could not be fetched"
                                },
                            );
                        }
                        load.index
                    }
                    Err(_) => {
                        self.indexes
                            .skip(SourceId::Wowi, "its file list could not be fetched");
                        None
                    }
                }
            })
            .as_ref()
    }
}

impl TukuiDirs for IndexLookups<'_, '_> {
    fn by_directory(&self, folder: &FolderName) -> Vec<TukuiHit> {
        self.tukui_list()
            .into_iter()
            .flat_map(|list| &list.entries)
            .filter(|e| {
                e.directories
                    .iter()
                    .any(|d| d.eq_ignore_ascii_case(folder.as_str()))
            })
            .map(|e| TukuiHit {
                slug: e.slug.clone(),
                name: e.name.clone(),
            })
            .collect()
    }
}

impl WowiDirs for IndexLookups<'_, '_> {
    fn by_dir(&self, folder: &FolderName) -> Vec<WowiProject> {
        let Some(index) = self.wowi_index() else {
            return Vec::new();
        };
        index
            .by_folder(folder)
            .iter()
            .filter_map(|uid| index.by_uid(*uid))
            .map(|e| {
                // What the author declares lags: a ranking hint only.
                let mut families: Vec<Family> = e
                    .compat
                    .iter()
                    .filter_map(|c| c.version.trim().parse::<ClientVersion>().ok())
                    .map(|v| self.ctx.table.family_for(&v, InstallKind::Battlenet))
                    .filter(|f| *f != Family::Unknown)
                    .collect();
                families.sort();
                families.dedup();
                WowiProject {
                    uid: e.uid,
                    name: e.name.clone(),
                    dirs: e.dirs.clone(),
                    families,
                    downloads: e.downloads,
                    date: i64::try_from(e.date_ms)
                        .ok()
                        .and_then(|ms| Timestamp::from_millisecond(ms).ok()),
                }
            })
            .collect()
    }
}
