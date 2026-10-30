//! The seven rows `status` publishes, detected from the view's layers: what each reads,
//! and that each says what `status` said before the registry (the legacy oracle).

#![cfg(test)]

use std::collections::BTreeMap;

use crate::app::drift::DriftItem;
use crate::app::issues::{self, Issue, legacy};
use crate::app::status::{Attention, test_target};
use crate::doctor::budget::Budget;
use crate::doctor::finding::Finding;
use crate::doctor::registry::{self, Layer, ids};
use crate::doctor::selection::Selection;
use crate::doctor::view::tests::{
    K, K2, Plant, client, committed_journal, home, key, plant, registry_bytes, world_of,
};
use crate::doctor::view::{DoctorView, LayerState, Preloaded, Records, Unavailable, WidgetLayer};
use crate::drift::DriftState;
use crate::fsx::GlobalLock;
use crate::model::{
    AddonReason, AddonRecord, AddonState, Channel, FolderName, TxId, VersionConfidence,
};
use crate::omarchy::status::WidgetInfo;
use crate::state::files_index::{FileEntry, FilesIndex, FolderIndex};
use crate::state::paths::Paths;
use crate::state::records::TargetRecord;
use crate::state::store::{self, Loaded, SchemaVersion, VersionStatus, Versioned};

/// `id`'s row run on `view`, its observations made findings.
fn detected(view: &DoctorView, id: &str) -> Vec<Finding> {
    let row = registry::row(id).unwrap();
    (row.detect.unwrap())(view, &Selection::all(), &mut Budget::unlimited())
        .found
        .into_iter()
        .map(|obs| row.finding(obs))
        .collect()
}

#[test]
fn moved_rows_require_their_layers() {
    let cases: [(&str, &[Layer]); 7] = [
        (ids::JOURNAL_PENDING, &[Layer::Journals, Layer::Lock]),
        (ids::STATE_VERSION_NEWER, &[Layer::Records]),
        (
            ids::RECORD_FOLDER_MISSING,
            &[Layer::Discovery, Layer::Records],
        ),
        (
            ids::MANAGED_FILES_MODIFIED,
            &[Layer::Discovery, Layer::Records],
        ),
        (ids::OWNERSHIP_TRANSFERRED, &[Layer::Records]),
        (ids::TARGET_VANISHED, &[Layer::Discovery]),
        (ids::WIDGET_VERSION_DRIFT, &[Layer::Widget]),
    ];
    for (id, layers) in cases {
        assert_eq!(registry::row(id).unwrap().requires, layers, "{id}");
    }
}

fn addon(name: &str, state: AddonState) -> AddonRecord {
    AddonRecord {
        r#ref: "gh:example/Foo".parse().unwrap(),
        name: name.into(),
        version: "1.0".into(),
        version_confidence: VersionConfidence::Release,
        release_id: None,
        channel: Channel::Stable,
        pinned: false,
        reason: AddonReason::Explicit,
        required_by: vec![],
        folders: vec![FolderName::new(name).unwrap()],
        state,
        installed_at: "2026-10-14T21:00:00Z".parse().unwrap(),
        tx_id: "01JB8Q2VA00000000000000000".parse().unwrap(),
        blob_sha256: None,
        revision: None,
        left_out: Vec::new(),
        any_flavor: false,
    }
}

/// `key`'s record holding `addons`, each with one folder of its name.
fn record(k: &str, addons: &[(&str, AddonState)]) -> TargetRecord {
    TargetRecord {
        generation: 1,
        addons: addons
            .iter()
            .map(|(name, state)| (TargetRecord::addon_key(name), addon(name, *state)))
            .collect(),
        ..TargetRecord::empty(key(k))
    }
}

#[test]
fn moved_detectors_run_from_their_layers_alone() {
    fn unavailable<T>(layer: Layer, cause: Layer) -> Option<LayerState<T>> {
        Some(LayerState::Unavailable(Unavailable { layer, cause }))
    }
    let (_dir, paths) = home();
    let newer = Loaded {
        value: record(K, &[]),
        version: SchemaVersion::new(1, 9),
        status: VersionStatus::Newer,
    };
    let degraded = Loaded {
        value: record(K2, &[("Foo", AddonState::Degraded)]),
        version: TargetRecord::CURRENT,
        status: VersionStatus::Current,
    };
    let records = Records::from_states(BTreeMap::from([
        (key(K), LayerState::Available(newer)),
        (key(K2), LayerState::Available(degraded)),
    ]));
    let view = DoctorView::preloaded(
        paths,
        Preloaded {
            config: unavailable(Layer::Config, Layer::Config),
            registry: unavailable(Layer::Registry, Layer::Registry),
            world: unavailable(Layer::Discovery, Layer::Config),
            records: Some(LayerState::Available(records)),
            ..Preloaded::default()
        },
    );
    let found: Vec<(&str, String)> = [ids::STATE_VERSION_NEWER, ids::OWNERSHIP_TRANSFERRED]
        .iter()
        .flat_map(|id| detected(&view, id))
        .map(|f| (f.id, f.target.map(|t| t.id).unwrap_or_default()))
        .collect();
    assert_eq!(
        found,
        [
            (ids::STATE_VERSION_NEWER, K.to_owned()),
            (ids::OWNERSHIP_TRANSFERRED, K2.to_owned()),
        ],
        "named by key: the registry cannot be read"
    );
}

/// A home whose registry names `K` retail.
fn retail_home() -> (tempfile::TempDir, Paths) {
    let (dir, paths) = home();
    plant(
        &paths.registry_file(),
        &Plant::Bytes(registry_bytes(&[(K, "retail")])),
    );
    (dir, paths)
}

/// What `status` said about retail (`K`) before the registry: the legacy rules over a
/// target with these facts.
fn oracle(
    attention: &[Attention],
    degraded: usize,
    drift: Vec<DriftItem>,
    reason: Option<&str>,
) -> Vec<Issue> {
    let mut target = test_target("retail", 0, attention, 0);
    target.key = Some(key(K));
    target.addons.degraded = degraded;
    target.drift = drift;
    legacy::issues_for(&target, reason)
}

fn drift(state: DriftState, files: u64) -> Vec<DriftItem> {
    vec![DriftItem {
        addon: "Foo".to_owned(),
        state,
        files,
        folders: None,
    }]
}

/// Retail (`K`) discovered at its client directory, with `Interface/AddOns/` created.
fn with_retail(paths: &Paths) -> DoctorView {
    let client_dir = paths.home().join("Games/wow/_retail_");
    std::fs::create_dir_all(client_dir.join("Interface/AddOns")).unwrap();
    let world = world_of(paths, vec![client("retail", &client_dir, Some(K))]);
    DoctorView::preloaded(
        paths.clone(),
        Preloaded {
            world: Some(LayerState::Available(world)),
            ..Preloaded::default()
        },
    )
}

#[test]
fn moved_rules_match_status() {
    let tx: TxId = "01M7A1B2C3D4E5F6G7H8J9K0MP".parse().unwrap();
    let journal = |paths: &Paths, text: String| {
        plant(
            &paths.journal_dir().join(format!("{tx}.jsonl")),
            &Plant::Bytes(text.into_bytes()),
        );
    };
    let prepared = || {
        let text = committed_journal(tx, key(K));
        format!("{}\n", text.lines().next().unwrap())
    };
    let on_retail = |view: &DoctorView, id| issues::for_target(&detected(view, id), key(K));

    let (_dir, paths) = retail_home();
    journal(&paths, prepared());
    let view = DoctorView::load_at(paths.clone(), Default::default());
    assert_eq!(
        on_retail(&view, ids::JOURNAL_PENDING),
        oracle(&[Attention::JournalPending], 0, Vec::new(), None),
        "case journal_pending_free_lock"
    );
    let guard = GlobalLock::acquire(&paths, "test").unwrap();
    let held = DoctorView::load_at(paths.clone(), Default::default());
    let found = on_retail(&held, ids::JOURNAL_PENDING);
    drop(guard);
    assert_eq!(
        found,
        oracle(&[], 0, Vec::new(), None),
        "case journal_pending_held_lock"
    );

    for (case, version, reason) in [
        ("journal_unreadable", "1.1", "journal_unreadable"),
        ("journal_too_new", "9.0", "journal_too_new"),
    ] {
        let (_dir, paths) = retail_home();
        journal(
            &paths,
            format!(
                "{{\"type\":\"prepared\",\"schema_version\":\"{version}\",\"target_key\":\"{K}\"}}\n"
            ),
        );
        let view = DoctorView::load_at(paths, Default::default());
        assert_eq!(
            on_retail(&view, ids::JOURNAL_PENDING),
            oracle(&[Attention::JournalPending], 0, Vec::new(), Some(reason)),
            "case {case}"
        );
    }

    let (_dir, paths) = retail_home();
    journal(&paths, "not a journal\n".to_owned());
    let view = DoctorView::load_at(paths, Default::default());
    assert_eq!(
        issues::for_machine(&detected(&view, ids::JOURNAL_PENDING)),
        legacy::machine_issues(None, true),
        "case journal_unattributed"
    );

    let (_dir, paths) = retail_home();
    let newer = format!(r#"{{"schema_version":"1.9","key":"{K}","generation":1,"addons":{{}}}}"#);
    plant(
        &paths.target_record(&key(K)),
        &Plant::Bytes(newer.into_bytes()),
    );
    let view = DoctorView::load_at(paths, Default::default());
    assert_eq!(
        on_retail(&view, ids::STATE_VERSION_NEWER),
        oracle(&[Attention::StateVersionNewer], 0, Vec::new(), None),
        "case state_version_newer"
    );

    for (case, n) in [
        ("ownership_transferred_one", 1),
        ("ownership_transferred_two", 2),
    ] {
        let (_dir, paths) = retail_home();
        let names = ["Foo", "Bar"];
        let degraded: Vec<(&str, AddonState)> = names[..n]
            .iter()
            .map(|name| (*name, AddonState::Degraded))
            .collect();
        let bytes = store::encode(&record(K, &degraded)).unwrap();
        plant(&paths.target_record(&key(K)), &Plant::Bytes(bytes));
        let view = DoctorView::load_at(paths, Default::default());
        assert_eq!(
            on_retail(&view, ids::OWNERSHIP_TRANSFERRED),
            oracle(&[Attention::AddonDegraded], n, Vec::new(), None),
            "case {case}"
        );
    }

    let (_dir, paths) = retail_home();
    let bytes = store::encode(&record(K, &[("Foo", AddonState::Ok)])).unwrap();
    plant(&paths.target_record(&key(K)), &Plant::Bytes(bytes));
    let view = with_retail(&paths);
    assert_eq!(
        on_retail(&view, ids::RECORD_FOLDER_MISSING),
        oracle(
            &[Attention::AddonFolderMissing],
            0,
            drift(DriftState::Missing, 0),
            None
        ),
        "case record_folder_missing"
    );

    for (case, n) in [
        ("managed_files_modified_one_file", 1),
        ("managed_files_modified_three_files", 3),
    ] {
        let (_dir, paths) = retail_home();
        let bytes = store::encode(&record(K, &[("Foo", AddonState::Ok)])).unwrap();
        plant(&paths.target_record(&key(K)), &Plant::Bytes(bytes));
        let view = with_retail(&paths);
        let folder = paths.home().join("Games/wow/_retail_/Interface/AddOns/Foo");
        // Each file on disk differs in size from what the index recorded.
        let entries: Vec<FileEntry> = (0..n)
            .map(|i| {
                let name = format!("f{i}.lua");
                plant(
                    &folder.join(&name),
                    &Plant::Bytes(b"edited by hand".to_vec()),
                );
                FileEntry {
                    path: name,
                    size: 3,
                    mtime_ns: 0,
                    sha256: "00".repeat(32),
                }
            })
            .collect();
        let index = FilesIndex {
            key: key(K),
            written_at_ns: 0,
            folders: BTreeMap::from([("foo".to_owned(), FolderIndex::new("Foo", entries))]),
        };
        plant(
            &paths.files_index(&key(K)),
            &Plant::Bytes(store::encode(&index).unwrap()),
        );
        assert_eq!(
            on_retail(&view, ids::MANAGED_FILES_MODIFIED),
            oracle(
                &[Attention::AddonModified],
                0,
                drift(DriftState::Modified, n),
                None
            ),
            "case {case}"
        );
    }

    let (_dir, paths) = retail_home();
    let mut world = world_of(&paths, Vec::new());
    world.resolved.missing = vec![crate::state::registry::RegistryEntry {
        key: key(K),
        id: "retail".to_owned(),
        install_root: paths.home().join("gone"),
        client_dir: paths.home().join("gone/_retail_"),
        flavor_dir: Some("_retail_".to_owned()),
        product: Some("wow".to_owned()),
        qualifier: "test".to_owned(),
        origin: crate::state::registry::Origin::Discovered,
        registered_at: "2026-10-14T12:00:00Z".parse().unwrap(),
    }];
    let view = DoctorView::preloaded(
        paths,
        Preloaded {
            world: Some(LayerState::Available(world)),
            ..Preloaded::default()
        },
    );
    assert_eq!(
        on_retail(&view, ids::TARGET_VANISHED),
        oracle(&[Attention::TargetMissing], 0, Vec::new(), None),
        "case target_vanished"
    );

    let (_dir, paths) = home();
    let info = WidgetInfo {
        omarchy: true,
        installed: true,
        enabled: true,
        version: Some("0.0.9".to_owned()),
        outdated: true,
        context_matches: Some(true),
    };
    let view = DoctorView::preloaded(
        paths,
        Preloaded {
            widget: Some(LayerState::Available(WidgetLayer {
                info: Some(info.clone()),
            })),
            ..Preloaded::default()
        },
    );
    assert_eq!(
        issues::for_machine(&detected(&view, ids::WIDGET_VERSION_DRIFT)),
        legacy::machine_issues(Some(&info), false),
        "case widget_drift"
    );
}
