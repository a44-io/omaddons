//! `__complete`'s readers over synthetic state files: each kind's values, their order and
//! filters, and nothing at all for a missing, damaged or newer source.
#![cfg(test)]

use std::path::Path;

use clap::ValueEnum;
use jiff::Timestamp;
use ulid::Ulid;

use super::*;
use crate::backup::inventory::{InventoryFile, TargetInventory};
use crate::doctor::runtime::DoctorRunId;
use crate::model::{
    AddonReason, AddonRecord, AddonState, BackupId, Channel, FolderName, PlanId, TargetKey,
    VersionConfidence,
};
use crate::state::records::TargetRecord;
use crate::state::registry::Origin;
use crate::state::store::Versioned as _;

struct Fixture {
    _dir: tempfile::TempDir,
    paths: Paths,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let paths = Paths::with_roots(&root, root.join("c"), root.join("s"), root.join("k"));
        Self { _dir: dir, paths }
    }

    fn write(&self, path: &Path, bytes: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    /// Registers `ids`, each with a new key, and returns the keys in order.
    fn register(&self, ids: &[&str]) -> Vec<TargetKey> {
        let entries: Vec<RegistryEntry> = ids.iter().map(|id| entry(id)).collect();
        let keys = entries.iter().map(|e| e.key).collect();
        let registry = Registry {
            targets: entries,
            default: None,
        };
        self.write(
            &self.paths.registry_file(),
            &store::encode(&registry).unwrap(),
        );
        keys
    }

    fn record(&self, key: TargetKey, names: &[&str]) {
        let mut record = TargetRecord::empty(key);
        for name in names {
            record
                .addons
                .insert(TargetRecord::addon_key(name), addon(name));
        }
        self.write(
            &self.paths.target_record(&key),
            &store::encode(&record).unwrap(),
        );
    }

    /// A discovery cache of one root holding `found` (`(default id, client dir name)`), in
    /// that order, as a plain `status` writes it.
    fn cache(&self, found: &[(&str, &str)]) {
        let targets = found
            .iter()
            .map(|(id, dir)| {
                let mut d = crate::doctor::view::tests::client(id, Path::new(dir), None);
                d.qualifier = format!("q-{id}");
                d
            })
            .collect();
        let cache = DiscoveryCache {
            basis: "test".to_owned(),
            roots: vec![crate::discover::cache::CachedRoot {
                path: "/games".into(),
                canonical: "/games".into(),
                kind: crate::discover::scan::RootKind::Battlenet,
                source: crate::discover::DiscoverySource::Scan,
                qualifier: "test".to_owned(),
                hints: crate::discover::battlenet::BattlenetHints::default(),
                hint_files: Vec::new(),
                stamps: Vec::new(),
                listing_sha256: String::new(),
                targets,
                warnings: Vec::new(),
            }],
        };
        self.write(
            &self.paths.discovery_cache(),
            &store::encode(&cache).unwrap(),
        );
    }

    /// `__complete` with these arguments, its values.
    fn complete(&self, args: &[&str]) -> Vec<String> {
        let args: Vec<OsString> = args.iter().map(OsString::from).collect();
        run(&args, Some(&self.paths)).values().to_vec()
    }
}

fn entry(id: &str) -> RegistryEntry {
    RegistryEntry {
        key: TargetKey::new(),
        id: (*id).to_owned(),
        install_root: format!("/games/{id}").into(),
        client_dir: format!("/games/{id}/_retail_").into(),
        flavor_dir: Some("_retail_".to_owned()),
        product: None,
        qualifier: String::new(),
        origin: Origin::Manual,
        registered_at: Timestamp::UNIX_EPOCH,
    }
}

fn addon(name: &str) -> AddonRecord {
    AddonRecord {
        r#ref: "gh:WeakAuras/WeakAuras2".parse().unwrap(),
        name: name.into(),
        version: "1.0.0".into(),
        version_confidence: VersionConfidence::Release,
        release_id: None,
        channel: Channel::Stable,
        pinned: false,
        reason: AddonReason::Explicit,
        required_by: vec![],
        folders: vec![FolderName::new("Folder").unwrap()],
        state: AddonState::Ok,
        installed_at: "2026-10-14T21:00:00Z".parse().unwrap(),
        tx_id: "01JB8Q2VA00000000000000000".parse().unwrap(),
        blob_sha256: None,
        revision: None,
        left_out: Vec::new(),
        any_flavor: false,
    }
}

/// A ULID `n` milliseconds into 2026, so that a larger `n` is newer.
fn ulid(n: u64) -> Ulid {
    Ulid::from_parts(1_767_225_600_000 + n, u128::from(n))
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|v| (*v).to_owned()).collect()
}

#[test]
fn kind_words_are_the_parsers_values_in_order() {
    let words: Vec<String> = Kind::value_variants()
        .iter()
        .filter_map(ValueEnum::to_possible_value)
        .map(|v| v.get_name().to_owned())
        .collect();
    let ours: Vec<String> = Kind::ALL.iter().map(|k| k.word().to_owned()).collect();
    assert_eq!(words, ours);
    assert_eq!(
        ours,
        [
            "target",
            "client",
            "addon",
            "plan",
            "backup",
            "tx",
            "run",
            "doctor-run",
            "fm"
        ]
    );
}

#[test]
fn only_the_exact_first_argument_is_an_invocation() {
    for (case, argv, expected) in [
        ("the word", &["omaddons", "__complete", "target"][..], true),
        ("the word alone", &["omaddons", "__complete"], true),
        ("no argument", &["omaddons"], false),
        ("a longer word", &["omaddons", "__completes"], false),
        (
            "the public command",
            &["omaddons", "completions", "bash"],
            false,
        ),
        ("after a flag", &["omaddons", "--json", "__complete"], false),
        (
            "as a later word",
            &["omaddons", "status", "__complete"],
            false,
        ),
        ("another case", &["omaddons", "__COMPLETE"], false),
    ] {
        let raw: Vec<OsString> = argv.iter().map(OsString::from).collect();
        assert_eq!(is_invocation(&raw), expected, "{case}: {argv:?}");
    }
}

#[test]
fn arguments_that_do_not_parse_or_no_paths_offer_nothing() {
    let fixture = Fixture::new();
    fixture.register(&["retail"]);
    for (case, args) in [
        ("no kind", &[][..]),
        ("an unknown kind", &["game"]),
        ("two kinds", &["target", "addon"]),
        ("an unknown flag", &["target", "--json"]),
        ("help", &["target", "--help"]),
    ] {
        assert_eq!(fixture.complete(args), Vec::<String>::new(), "{case}");
    }
    let args = [OsString::from("target")];
    assert!(run(&args, None).values().is_empty(), "no paths");
    assert_eq!(fixture.complete(&["target"]), strings(&["retail"]));
}

#[test]
fn every_kind_offers_nothing_in_an_empty_home() {
    let fixture = Fixture::new();
    for kind in Kind::ALL {
        let values = fixture.complete(&[kind.word()]);
        if kind == Kind::Fm {
            assert_eq!(values.len(), FAILURE_MODES.len(), "fm is compiled in");
        } else {
            assert_eq!(values, Vec::<String>::new(), "{kind:?}");
        }
    }
}

#[test]
fn targets_are_the_registered_ids_sorted_and_filtered_by_prefix() {
    let fixture = Fixture::new();
    fixture.register(&["retail", "Era", "wotlk", "retail@steam"]);
    for (case, args, expected) in [
        (
            "all",
            &["target"][..],
            &["Era", "retail", "retail@steam", "wotlk"][..],
        ),
        (
            "a prefix",
            &["target", "--prefix", "re"],
            &["retail", "retail@steam"],
        ),
        (
            "ignoring case",
            &["target", "--prefix", "RETAIL@"],
            &["retail@steam"],
        ),
        (
            "upper id, lower prefix",
            &["target", "--prefix=e"],
            &["Era"],
        ),
        ("no match", &["target", "--prefix", "x"], &[]),
        ("a hyphen prefix", &["target", "--prefix", "-t"], &[]),
    ] {
        assert_eq!(fixture.complete(args), strings(expected), "{case}");
    }
}

#[test]
fn a_registry_that_cannot_be_read_offers_no_target_and_no_addon() {
    let fixture = Fixture::new();
    let keys = fixture.register(&["retail"]);
    fixture.record(keys[0], &["WeakAuras"]);
    let path = fixture.paths.registry_file();
    let valid = std::fs::read(&path).unwrap();
    let text = String::from_utf8(valid.clone()).unwrap();
    let newer = text.replacen(
        "\"schema_version\": \"1.0\"",
        "\"schema_version\": \"9.0\"",
        1,
    );
    assert_ne!(newer, text, "the registry names its version");
    let mut oversized = valid;
    oversized.resize(crate::limits::DISCOVERY_FILE_MAX_BYTES as usize + 1, b' ');
    for (case, bytes) in [
        ("unparsable", b"{ not json".to_vec()),
        ("newer", newer.into_bytes()),
        ("oversized", oversized),
    ] {
        fixture.write(&path, &bytes);
        assert_eq!(
            fixture.complete(&["target"]),
            Vec::<String>::new(),
            "{case}"
        );
        assert_eq!(fixture.complete(&["addon"]), Vec::<String>::new(), "{case}");
        // Nor a client: the cache's provisional ids could not be told from registered ones.
        fixture.cache(&[("era", "/games/era/_classic_era_")]);
        assert_eq!(
            fixture.complete(&["client"]),
            Vec::<String>::new(),
            "{case}"
        );
    }
}

/// `client` is what `-t` takes: the registered ids, sorted, then the ids the discovery
/// cache's unregistered clients resolve to, in its order, as `reconcile` gives them (a
/// second client with the same default id is qualified, as `-t` names it). A cached
/// client that is registered is offered once, by its registered id. `target` stays the
/// registered ids alone. A fresh install, with no registry yet, offers the cache's.
#[test]
fn clients_are_the_registered_ids_then_the_cached_ones_as_minus_t_names_them() {
    let fixture = Fixture::new();
    fixture.cache(&[
        ("retail", "/games/retail/_retail_"),
        ("retail", "/games/steam/_retail_"),
        ("era", "/games/era/_classic_era_"),
    ]);
    assert_eq!(
        fixture.complete(&["client"]),
        strings(&["retail", "retail@q-retail", "era"]),
        "a fresh install: no registry yet"
    );
    assert_eq!(fixture.complete(&["target"]), Vec::<String>::new());

    // `era` registered (its client dir is the cached one's), `wotlk` registered elsewhere.
    let mut era = entry("era");
    era.client_dir = "/games/era/_classic_era_".into();
    let registry = Registry {
        targets: vec![entry("wotlk"), era],
        default: None,
    };
    fixture.write(
        &fixture.paths.registry_file(),
        &store::encode(&registry).unwrap(),
    );
    assert_eq!(
        fixture.complete(&["client"]),
        strings(&["era", "wotlk", "retail", "retail@q-retail"])
    );
    assert_eq!(fixture.complete(&["target"]), strings(&["era", "wotlk"]));
    assert_eq!(
        fixture.complete(&["client", "--prefix", "RETAIL@"]),
        strings(&["retail@q-retail"])
    );
}

/// A discovery cache that is missing, cannot be read, is oversized or a newer omaddons
/// wrote adds nothing to the registered ids.
#[test]
fn a_cache_that_cannot_be_read_adds_no_client() {
    let fixture = Fixture::new();
    fixture.register(&["wotlk"]);
    assert_eq!(
        fixture.complete(&["client"]),
        strings(&["wotlk"]),
        "no cache"
    );
    fixture.cache(&[("retail", "/games/retail/_retail_")]);
    let path = fixture.paths.discovery_cache();
    let valid = std::fs::read(&path).unwrap();
    let text = String::from_utf8(valid.clone()).unwrap();
    let current = format!("\"schema_version\": \"{}\"", DiscoveryCache::CURRENT);
    let newer = text.replacen(&current, "\"schema_version\": \"99.0\"", 1);
    assert_ne!(newer, text, "the cache names its version");
    let mut oversized = valid;
    oversized.resize(crate::limits::DISCOVERY_FILE_MAX_BYTES as usize + 1, b' ');
    for (case, bytes) in [
        ("unparsable", b"{ not json".to_vec()),
        ("newer", newer.into_bytes()),
        ("oversized", oversized),
    ] {
        fixture.write(&path, &bytes);
        assert_eq!(fixture.complete(&["client"]), strings(&["wotlk"]), "{case}");
    }
}

#[test]
fn addons_are_one_clients_names_or_every_clients() {
    let fixture = Fixture::new();
    let keys = fixture.register(&["retail", "era", "wotlk"]);
    fixture.record(keys[0], &["WeakAuras", "DBM-Core", "Details"]);
    fixture.record(keys[1], &["Questie", "details"]);
    // wotlk has no record: it adds nothing.
    let retail_key = keys[0].to_string();
    for (case, args, expected) in [
        (
            "one client",
            &["addon", "--target", "retail"][..],
            &["DBM-Core", "Details", "WeakAuras"][..],
        ),
        (
            "by key",
            &["addon", "--target", &retail_key],
            &["DBM-Core", "Details", "WeakAuras"],
        ),
        (
            "ignoring case",
            &["addon", "--target=RETAIL"],
            &["DBM-Core", "Details", "WeakAuras"],
        ),
        (
            "another client",
            &["addon", "--target", "era"],
            &["details", "Questie"],
        ),
        (
            "a client with no record",
            &["addon", "--target", "wotlk"],
            &[],
        ),
        (
            "every client, each name once",
            &["addon"],
            &["DBM-Core", "Details", "Questie", "WeakAuras"],
        ),
        (
            "a selector that is no registered id",
            &["addon", "--target", "sod"],
            &["DBM-Core", "Details", "Questie", "WeakAuras"],
        ),
        (
            "a prefix",
            &["addon", "--target", "retail", "--prefix", "d"],
            &["DBM-Core", "Details"],
        ),
    ] {
        assert_eq!(fixture.complete(args), strings(expected), "{case}");
    }
}

#[test]
fn a_record_that_cannot_be_read_adds_no_name() {
    let fixture = Fixture::new();
    let keys = fixture.register(&["retail", "era"]);
    fixture.record(keys[0], &["WeakAuras"]);
    fixture.write(&fixture.paths.target_record(&keys[1]), b"{ not json");
    assert_eq!(fixture.complete(&["addon"]), strings(&["WeakAuras"]));
}

#[test]
fn a_value_with_a_control_character_is_never_offered() {
    let fixture = Fixture::new();
    let keys = fixture.register(&["retail", "bad\nid"]);
    fixture.record(keys[0], &["Good", "Bad\tName", "Line\nBreak"]);
    assert_eq!(fixture.complete(&["target"]), strings(&["retail"]));
    assert_eq!(fixture.complete(&["addon"]), strings(&["Good"]));
}

#[test]
fn plans_are_the_stored_plan_ids_newest_first() {
    let fixture = Fixture::new();
    let dir = fixture.paths.plans_dir();
    let ids: Vec<String> = (1..=3)
        .map(|n| PlanId::from_ulid(ulid(n)).to_string())
        .collect();
    for id in &ids {
        fixture.write(&dir.join(format!("{id}.json")), b"{}");
    }
    fixture.write(&dir.join("notes.txt"), b"not a plan");
    fixture.write(&dir.join("NOT-A-ULID.json"), b"{}");
    let newest_first: Vec<String> = ids.iter().rev().cloned().collect();
    assert_eq!(fixture.complete(&["plan"]), newest_first);
    let prefix = &ids[0][..20];
    assert!(
        fixture
            .complete(&["plan", "--prefix", &prefix.to_ascii_lowercase()])
            .contains(&ids[0]),
        "a lowercase prefix of {}",
        ids[0]
    );
}

#[test]
fn backups_are_one_clients_or_every_clients_newest_first() {
    let fixture = Fixture::new();
    let keys = fixture.register(&["retail", "era"]);
    let id = |n| BackupId::from_ulid(ulid(n)).to_string();
    let mut file = InventoryFile::default();
    for (key, ns) in [(keys[0], [1, 3]), (keys[1], [2, 4])] {
        let mut inventory = TargetInventory::default();
        inventory.covers.backups = ns.iter().map(|n| BackupId::from_ulid(ulid(*n))).collect();
        file.targets.insert(key, inventory);
    }
    fixture.write(
        &fixture.paths.backup_inventory(),
        &store::encode(&file).unwrap(),
    );
    assert_eq!(
        fixture.complete(&["backup", "--target", "retail"]),
        vec![id(3), id(1)]
    );
    assert_eq!(
        fixture.complete(&["backup"]),
        vec![id(4), id(3), id(2), id(1)]
    );
    fixture.write(&fixture.paths.backup_inventory(), b"{ not json");
    assert_eq!(fixture.complete(&["backup"]), Vec::<String>::new());
}

#[test]
fn doctor_runs_are_latest_then_the_published_runs_newest_first() {
    let fixture = Fixture::new();
    let dir = fixture.paths.doctor_runs_dir();
    let at = |s: &str| s.parse::<Timestamp>().unwrap();
    let older = DoctorRunId::new(at("2026-10-30T18:03:40Z"), ulid(1));
    let newer = DoctorRunId::new(at("2026-11-02T09:00:00Z"), ulid(2));
    let staged = DoctorRunId::new(at("2026-11-03T09:00:00Z"), ulid(3));
    for name in [
        older.as_str().to_owned(),
        newer.as_str().to_owned(),
        format!(".{}.tmp", staged.as_str()),
        "not-a-run".to_owned(),
    ] {
        std::fs::create_dir_all(dir.join(name)).unwrap();
    }
    assert_eq!(
        fixture.complete(&["doctor-run"]),
        strings(&["latest", newer.as_str(), older.as_str()])
    );
    assert_eq!(
        fixture.complete(&["doctor-run", "--prefix", "2026-10"]),
        strings(&[older.as_str()])
    );
}

#[test]
fn failure_modes_are_every_id_and_a_list_keeps_what_it_has() {
    let fixture = Fixture::new();
    let all: Vec<String> = FAILURE_MODES.iter().map(|fm| fm.id.to_owned()).collect();
    assert_eq!(fixture.complete(&["fm"]), all);
    let first = all[0].clone();
    let caches: Vec<String> = all
        .iter()
        .filter(|id| id.starts_with("fm-caches-"))
        .map(|id| format!("{first},{id}"))
        .collect();
    assert!(!caches.is_empty(), "the registry has cache checks");
    assert_eq!(
        fixture.complete(&["fm", "--prefix", &format!("{first},fm-caches-")]),
        caches
    );
    // A list item is never offered for any other kind.
    assert_eq!(
        fixture.complete(&["target", "--prefix", "a,b"]),
        Vec::<String>::new()
    );
}

#[test]
fn values_stop_at_the_bound() {
    let fixture = Fixture::new();
    let ids: Vec<String> = (0..=COMPLETION_MAX_VALUES)
        .map(|n| format!("client{n:05}"))
        .collect();
    let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
    fixture.register(&ids);
    let values = fixture.complete(&["target"]);
    assert_eq!(values.len(), COMPLETION_MAX_VALUES);
    assert_eq!(values.first().map(String::as_str), Some("client00000"));
}

/// The reader's own code runs no discovery, takes no lock, recovers nothing, makes no
/// request and writes nothing: it never names what does.
#[test]
fn the_reader_names_no_discovery_lock_recovery_network_or_writer() {
    use crate::source_scan::{Literals, production_code};

    let source = include_str!("../complete.rs");
    let production = production_code(source, Literals::Blank).unwrap_or_default();
    assert!(
        production.contains("fn values("),
        "the scan sees the reader's code"
    );
    for forbidden in [
        "World",
        "discover_",
        "DiscoveryMode",
        "discovery.json",
        "GlobalLock",
        "DirLock",
        "recover",
        "rebuild",
        "fsx::",
        "net::",
        "Catalog",
        "backup::list",
        "history::read",
        "std::fs::write",
        "std::process",
    ] {
        assert!(
            !production.contains(forbidden),
            "the reader must not use {forbidden}"
        );
    }
}

#[test]
fn the_human_form_is_one_value_per_line() {
    let outcome = CompleteOutcome {
        data: CompleteData {
            values: strings(&["era", "retail"]),
        },
    };
    let mut out = Vec::new();
    outcome.render_human(&mut out, &Style::default()).unwrap();
    assert_eq!(out, b"era\nretail\n");
    assert_eq!(outcome.command(), "__complete");
    let mut empty = Vec::new();
    CompleteOutcome::default()
        .render_human(&mut empty, &Style::default())
        .unwrap();
    assert!(empty.is_empty());
}
