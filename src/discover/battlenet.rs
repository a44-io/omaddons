//! A Battle.net WoW root (`.build.info` plus one `_<x>_/` flavor folder per installed
//! product) to one target per flavor folder (vision §4.1, §5.2 step 3).
//!
//! Detection is data-driven (D15): the folder's product comes from `.flavor.info`,
//! its version from `.build.info`, and family, interface and rules from the rules
//! table, so a new `_x_` folder becomes a usable target without code changes. The
//! product code never decides the family: `wow_classic_beta` runs Forever 1.60.x.

use std::collections::BTreeMap;
use std::path::Path;

use super::bpsv::{self, BuildInfoRow};
use super::{DiscoveredTarget, DiscoverySource, config_wtf, read_text};
use crate::error::Warning;
use crate::model::Target;
use crate::rules::{ClientVersion, Family, InstallKind, RulesTable, TargetKind};

/// What `product.db` knows about a root, passed in by the scan; empty without hints.
#[derive(
    Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct BattlenetHints {
    /// Flavor folder (e.g. `_forever_`) → product code.
    pub product_by_folder: BTreeMap<String, String>,
    /// Product code → version string.
    pub version_by_product: BTreeMap<String, String>,
}

/// Whether `dir` is a Battle.net root: `dir/.build.info` is a regular file.
pub fn is_battlenet_root(dir: &Path) -> bool {
    std::fs::metadata(dir.join(".build.info")).is_ok_and(|m| m.is_file())
}

/// Every flavor folder of `root` as a target, sorted by folder name, except those of
/// other Blizzard games ([`RulesTable::is_wow_product`]) and the leftovers of clients
/// Battle.net uninstalled ([`uninstalled`]), with the root's warnings: none today, since
/// a client's are said once the registry has named it, `unknown_flavor` once config
/// overrides applied ([`super::unknown_flavor`]) and `client_not_launched_since_patch`
/// from `DiscoveredTarget::not_launched_since` ([`super::not_launched_since_patch`]).
/// Reads only.
pub fn discover_root(
    root: &Path,
    table: &RulesTable,
    hints: &BattlenetHints,
    qualifier: &str,
    source: &DiscoverySource,
) -> (Vec<DiscoveredTarget>, Vec<Warning>) {
    let build_info = root.join(".build.info");
    // Read whole or not at all for the products it lists: a file cut at the bound
    // could leave out the row of an installed product.
    let whole = super::read_bytes(&build_info).map(|b| String::from_utf8_lossy(&b).into_owned());
    let listed = whole.as_deref().and_then(bpsv::build_info_products);
    let rows = whole
        .or_else(|| read_text(&build_info))
        .and_then(|text| bpsv::build_info_rows(&text).ok())
        .unwrap_or_default();
    let mut targets = Vec::new();
    for folder in flavor_folders(root) {
        let client_dir = root.join(&folder);
        let product = read_text(&client_dir.join(".flavor.info"))
            .and_then(|text| bpsv::flavor_product(&text))
            .or_else(|| hints.product_by_folder.get(&folder).cloned())
            .or_else(|| table.product_by_folder(&folder).map(|p| p.code.clone()));
        // Another Blizzard game's folder is skipped without a word; one whose product
        // cannot be read stays a target.
        if product.as_deref().is_some_and(|p| !table.is_wow_product(p)) {
            continue;
        }
        let known = product.as_deref().and_then(|p| table.product_by_code(p));
        let exe_name = exe_name(&client_dir, known.map(|p| p.exe.as_str()));
        // What Battle.net leaves of a client it uninstalled is no client, and is
        // skipped without a word too; a registered one is then a missing target.
        if uninstalled(
            &client_dir,
            &folder,
            product.as_deref(),
            listed.as_deref(),
            hints,
            exe_name.is_some(),
        ) {
            continue;
        }
        let version = version_for(product.as_deref(), &rows, hints)
            .and_then(|v| v.parse::<ClientVersion>().ok());

        let (version, family, rules) = match version {
            Some(v) => (
                v,
                table.family_for(&v, InstallKind::Battlenet),
                table.rules_for(&v, InstallKind::Battlenet).clone(),
            ),
            None => (
                ClientVersion::new(0, 0, 0),
                Family::Unknown,
                table.fallback(InstallKind::Battlenet).clone(),
            ),
        };
        let interface = super::client_interface(
            table,
            &client_dir,
            &version,
            family,
            InstallKind::Battlenet,
            &rules,
        );
        let kind = known.map_or_else(
            || {
                let test = |s: &str| {
                    let s = s.to_ascii_lowercase();
                    s.contains("ptr") || s.contains("beta")
                };
                if test(&folder) || product.as_deref().is_some_and(test) {
                    TargetKind::Test
                } else {
                    TargetKind::Live
                }
            },
            |p| p.kind,
        );
        let default_id = known.map_or_else(|| unknown_folder_id(&folder), |p| p.default_id.clone());

        // Only a `Config.wtf` that predates the client: launching it once clears that,
        // never a newer one (a version read from a stale hint). Recorded on the target and
        // said once the registry has named it (`super::super::not_launched_since_patch`).
        let not_launched_since = config_wtf::last_addon_version(&client_dir)
            .filter(|last| interface != 0 && *last < interface);

        targets.push(DiscoveredTarget {
            target: Target {
                key: None,
                id: default_id.clone(),
                install_root: root.to_path_buf(),
                client_dir,
                flavor_dir: Some(folder),
                product,
                install_kind: InstallKind::Battlenet,
                family,
                kind,
                version,
                interface,
                exe_name,
                label: Some(format!("{default_id} ({qualifier})")),
                judge: crate::toc::judge_digest(&rules, family, table),
                rules,
            },
            default_id,
            qualifier: qualifier.to_owned(),
            source: source.clone(),
            not_launched_since,
        });
    }
    (targets, Vec::new())
}

/// Whether `name` is a flavor folder's: `_<x>_`. Only these, inside a Battle.net root, are
/// its clients; any other folder there is judged on its own (omad-4h0d.19).
pub(crate) fn is_flavor_name(name: &str) -> bool {
    name.len() > 2 && name.starts_with('_') && name.ends_with('_')
}

/// Directories of `root` named `_<x>_`, sorted.
fn flavor_folders(root: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut folders: Vec<String> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| is_flavor_name(name))
        .collect();
    folders.sort();
    folders
}

/// Whether a flavor folder is what Battle.net leaves when it uninstalls a client (its
/// `Interface/`, fonts and shader caches), so no client at all. Every one of these must
/// hold; anything less certain keeps the folder a target, because hiding a live client
/// is the worst outcome:
/// - the folder can be listed, and holds no `.flavor.info` in any case (a client being
///   patched keeps its own);
/// - `.build.info` says which products are installed (`listed`, see
///   [`bpsv::build_info_products`]), and the folder's product is not one of them (a
///   client being installed has its row before its executable);
/// - the folder holds no client executable (`has_exe`, [`exe_name`]'s rule);
/// - no `product.db` hint names the folder or its product.
///
/// A folder whose product nothing names (`_xyz_`) is never judged uninstalled.
fn uninstalled(
    client_dir: &Path,
    folder: &str,
    product: Option<&str>,
    listed: Option<&[String]>,
    hints: &BattlenetHints,
    has_exe: bool,
) -> bool {
    let (Some(product), Some(listed)) = (product, listed) else {
        return false;
    };
    let named = |name: &String, wanted: &str| name.eq_ignore_ascii_case(wanted);
    std::fs::read_dir(client_dir).is_ok()
        && super::find_child(client_dir, ".flavor.info").is_none()
        && !listed.iter().any(|p| named(p, product))
        && !has_exe
        && !hints.product_by_folder.keys().any(|f| named(f, folder))
        && !hints
            .product_by_folder
            .values()
            .chain(hints.version_by_product.keys())
            .any(|p| named(p, product))
}

/// The folder's version string: its product's `.build.info` row (an active one when
/// there are several), else the hint, else the only row of a `.build.info` that names
/// no products (an old layout without the `Product` column).
fn version_for(
    product: Option<&str>,
    rows: &[BuildInfoRow],
    hints: &BattlenetHints,
) -> Option<String> {
    if let Some(product) = product {
        let mine: Vec<&BuildInfoRow> = rows
            .iter()
            .filter(|row| {
                row.product
                    .as_deref()
                    .is_some_and(|p| p.eq_ignore_ascii_case(product))
            })
            .collect();
        let row = mine.iter().find(|row| row.active).or_else(|| mine.first());
        if let Some(version) = row.and_then(|row| row.version.clone()) {
            return Some(version);
        }
        if let Some(version) = hints.version_by_product.get(product) {
            return Some(version.clone());
        }
    }
    match rows {
        [only] if only.product.is_none() => only.version.clone(),
        _ => None,
    }
}

/// The client executable's on-disk name: the product's exe ignoring ASCII case, else
/// the first `wow*.exe` other than `wowerror.exe`.
fn exe_name(client_dir: &Path, expected: Option<&str>) -> Option<String> {
    let mut files: Vec<String> = std::fs::read_dir(client_dir)
        .ok()?
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_file())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect();
    files.sort();
    if let Some(expected) = expected
        && let Some(name) = files.iter().find(|f| f.eq_ignore_ascii_case(expected))
    {
        return Some(name.clone());
    }
    files.into_iter().find(|f| {
        let lower = f.to_ascii_lowercase();
        lower.starts_with("wow") && lower.ends_with(".exe") && lower != "wowerror.exe"
    })
}

/// The default id of a folder the product table does not know: `_xyz_` → `xyz`.
fn unknown_folder_id(folder: &str) -> String {
    folder
        .strip_prefix('_')
        .and_then(|f| f.strip_suffix('_'))
        .unwrap_or(folder)
        .to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::time::SystemTime;

    use super::*;
    use crate::error::{Suggestion, WarningCode};

    const HEADER: &str = "Branch!STRING:0|Active!DEC:1|Build Key!HEX:16|Tags!STRING:0|Version!STRING:0|KeyRing!HEX:16|Product!STRING:0";

    /// product, folder, version, on-disk exe, expected family, interface, kind, id.
    type Case = (
        &'static str,
        &'static str,
        &'static str,
        &'static str,
        Family,
        u32,
        TargetKind,
        &'static str,
    );

    #[rustfmt::skip]
    const ROWS: &[Case] = &[
        ("wow",                 "_retail_",          "12.1.5.70391", "Wow.exe",         Family::Mainline, 120_105, TargetKind::Live, "retail"),
        ("wowt",                "_ptr_",             "12.1.0.70100", "WoWT.exe",        Family::Mainline, 120_100, TargetKind::Test, "ptr"),
        ("wowxptr",             "_xptr_",            "12.1.5.70400", "WoWT.exe",        Family::Mainline, 120_105, TargetKind::Test, "xptr"),
        ("wow_beta",            "_beta_",            "12.0.1.69000", "WoWB.exe",        Family::Mainline, 120_001, TargetKind::Test, "beta"),
        ("wow_classic",         "_classic_",         "5.5.4.63000",  "WoWClassic.exe",  Family::Mists,    50_504,  TargetKind::Live, "classic"),
        ("wow_classic_ptr",     "_classic_ptr_",     "5.5.4.63100",  "WoWClassicT.exe", Family::Mists,    50_504,  TargetKind::Test, "classic-ptr"),
        ("wow_classic_beta",    "_classic_beta_",    "1.60.1.70374", "WowB.exe",        Family::Forever,  16_001,  TargetKind::Test, "classic-beta"),
        ("wow_classic_era",     "_classic_era_",     "1.15.9.65000", "wowclassic.EXE",  Family::Vanilla,  11_509,  TargetKind::Live, "era"),
        ("wow_classic_era_ptr", "_classic_era_ptr_", "2.5.6.65100",  "WowClassicT.exe", Family::Tbc,      20_506,  TargetKind::Test, "era-ptr"),
        ("wow_anniversary",     "_anniversary_",     "2.5.6.65200",  "WoWClassic.exe",  Family::Tbc,      20_506,  TargetKind::Live, "anniversary"),
        ("wow_classic_titan",   "_classic_titan_",   "3.80.2.64000", "WoWClassic.exe",  Family::Titan,    38_002,  TargetKind::Live, "titan"),
        ("wowf",                "_forever_",         "1.60.1.70245", "Wow.exe",         Family::Forever,  16_001,  TargetKind::Live, "forever"),
    ];

    fn write(path: PathBuf, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn build_info(rows: &[(&str, &str, bool)]) -> String {
        let mut text = format!("{HEADER}\n");
        for (product, version, active) in rows {
            text.push_str(&format!(
                "us|{}|00ff|Windows x86_64 US? enUS speech?:Windows x86_64 US? enUS text?|{version}|00ff|{product}\n",
                u8::from(*active)
            ));
        }
        text
    }

    fn flavor(root: &Path, folder: &str, product: &str) {
        write(
            root.join(folder).join(".flavor.info"),
            &format!("Product Flavor!STRING:0\n{product}\n"),
        );
    }

    fn discover(root: &Path, hints: &BattlenetHints) -> (Vec<DiscoveredTarget>, Vec<Warning>) {
        discover_root(
            root,
            &RulesTable::builtin(),
            hints,
            "battlenet",
            &DiscoverySource::ProductDb,
        )
    }

    fn dump(targets: &[DiscoveredTarget], warnings: &[Warning]) -> String {
        format!(
            "{}\n{}",
            serde_json::to_string_pretty(targets).unwrap(),
            serde_json::to_string_pretty(warnings).unwrap()
        )
    }

    /// Every path under `dir` with its length and modification time.
    fn tree(dir: &Path) -> Vec<(PathBuf, u64, SystemTime)> {
        let mut out = Vec::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(at) = stack.pop() {
            for entry in fs::read_dir(&at).unwrap() {
                let path = entry.unwrap().path();
                let meta = fs::symlink_metadata(&path).unwrap();
                if meta.is_dir() {
                    stack.push(path.clone());
                }
                out.push((path, meta.len(), meta.modified().unwrap()));
            }
        }
        out.sort();
        out
    }

    #[test]
    fn every_product_of_the_vision_table() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let rows: Vec<(&str, &str, bool)> = ROWS.iter().map(|r| (r.0, r.2, true)).collect();
        write(root.join(".build.info"), &build_info(&rows));
        for &(product, folder, _, exe, ..) in ROWS {
            flavor(root, folder, product);
            write(root.join(folder).join(exe), "MZ");
            write(root.join(folder).join("WowError.exe"), "MZ");
            fs::create_dir_all(root.join(folder).join("Interface").join("AddOns")).unwrap();
        }
        fs::create_dir_all(root.join("Data")).unwrap();
        fs::create_dir_all(root.join("_")).unwrap();
        write(root.join("_file_"), "a file, not a flavor folder");
        let before = tree(root);

        let (targets, warnings) = discover(root, &BattlenetHints::default());
        let context = dump(&targets, &warnings);
        assert_eq!(targets.len(), 12, "{context}");
        assert!(warnings.is_empty(), "{context}");
        for &(product, folder, version, exe, family, interface, kind, id) in ROWS {
            let found = targets
                .iter()
                .find(|t| t.target.flavor_dir.as_deref() == Some(folder))
                .unwrap_or_else(|| panic!("{folder} missing\n{context}")); // ubs:ignore — test
            let t = &found.target;
            assert_eq!(
                (t.product.as_deref(), t.family, t.interface, t.kind),
                (Some(product), family, interface, kind),
                "{folder}\n{context}"
            );
            assert_eq!(t.version, version.parse().unwrap(), "{folder}");
            assert_eq!(t.exe_name.as_deref(), Some(exe), "{folder}");
            assert_eq!((t.id.as_str(), found.default_id.as_str()), (id, id));
            assert_eq!(
                t.label.as_deref(),
                Some(format!("{id} (battlenet)").as_str())
            );
            assert_eq!(t.client_dir, root.join(folder));
            assert_eq!((t.install_root.as_path(), t.key), (root, None));
            assert_eq!(t.install_kind, InstallKind::Battlenet);
            assert_eq!(t.rules.family, family, "{folder}: rules follow the family");
            assert_eq!(found.qualifier, "battlenet");
        }
        let folders: Vec<&str> = targets
            .iter()
            .filter_map(|t| t.target.flavor_dir.as_deref())
            .collect();
        let mut sorted = folders.clone();
        sorted.sort_unstable();
        assert_eq!(folders, sorted, "sorted by folder");
        assert_eq!(tree(root), before, "discovery writes nothing");
    }

    #[test]
    fn product_falls_back_to_hints_then_the_folder_table() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root.join(".build.info"),
            &build_info(&[
                ("wowf", "1.60.1.70245", true),
                ("wow", "12.1.5.70391", true),
            ]),
        );
        fs::create_dir_all(root.join("_launch_")).unwrap();
        fs::create_dir_all(root.join("_retail_")).unwrap();
        let hints = BattlenetHints {
            product_by_folder: BTreeMap::from([("_launch_".into(), "wowf".into())]),
            ..BattlenetHints::default()
        };
        let (targets, warnings) = discover(root, &hints);
        let context = dump(&targets, &warnings);
        let launch = &targets[0].target;
        assert_eq!(
            (launch.product.as_deref(), launch.family, launch.id.as_str()),
            (Some("wowf"), Family::Forever, "forever"),
            "{context}"
        );
        let retail = &targets[1].target;
        assert_eq!(
            (retail.product.as_deref(), retail.interface),
            (Some("wow"), 120_105),
            "{context}"
        );
        assert_eq!(
            launch.exe_name, None,
            "half-installed folders are still targets"
        );
    }

    /// What Battle.net leaves of `_classic_` once it uninstalled Mists Classic (2026-11-04,
    /// the development machine): omaddons' `Interface/` data, fonts and a loader DLL; no
    /// `.flavor.info`, no executable, no `WTF/`, and no `.build.info` row.
    fn ghost(root: &Path) {
        let dir = root.join("_classic_");
        write(
            dir.join("Interface").join(".omaddons").join("target.json"),
            "{}",
        );
        fs::create_dir_all(dir.join("Interface").join("AddOns").join("EllesmereUI")).unwrap();
        fs::create_dir_all(dir.join("Fonts")).unwrap();
        write(dir.join("libxell.dll"), "MZ");
        write(dir.join("WowClassic_loader.dll"), "MZ");
        write(dir.join("vkd3d-proton.cache"), "");
    }

    /// A live client: its `.flavor.info`, executable and `Interface/AddOns/`.
    fn live(root: &Path, folder: &str, product: &str, exe: &str) {
        flavor(root, folder, product);
        write(root.join(folder).join(exe), "MZ");
        fs::create_dir_all(root.join(folder).join("Interface").join("AddOns")).unwrap();
    }

    fn folders(targets: &[DiscoveredTarget]) -> Vec<&str> {
        targets
            .iter()
            .filter_map(|t| t.target.flavor_dir.as_deref())
            .collect()
    }

    /// An uninstalled client's folder is no target, and says nothing; the live folders
    /// beside it are found as before, and nothing is written.
    #[test]
    fn an_uninstalled_flavor_folder_is_no_target() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root.join(".build.info"),
            &build_info(&[
                ("wow", "12.1.5.70391", true),
                ("wow_classic_era", "1.15.9.70003", true),
            ]),
        );
        live(root, "_retail_", "wow", "Wow.exe");
        live(root, "_classic_era_", "wow_classic_era", "WoWClassic.exe");
        ghost(root);
        let before = tree(root);
        let (targets, warnings) = discover(root, &BattlenetHints::default());
        let context = dump(&targets, &warnings);
        assert_eq!(
            folders(&targets),
            ["_classic_era_", "_retail_"],
            "{context}"
        );
        assert!(warnings.is_empty(), "{context}");
        assert_eq!(tree(root), before, "discovery writes nothing");

        // Every client uninstalled but the one `.build.info` still lists.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root.join(".build.info"),
            &build_info(&[("wowf", "1.60.1.70790", true)]),
        );
        live(root, "_forever_", "wowf", "Wow.exe");
        ghost(root);
        for folder in ["_retail_", "_classic_era_", "_anniversary_"] {
            let dir = root.join(folder);
            fs::create_dir_all(dir.join("Interface").join("AddOns")).unwrap();
            write(dir.join("wow_loader.dll"), "MZ");
        }
        let (targets, warnings) = discover(root, &BattlenetHints::default());
        let context = dump(&targets, &warnings);
        assert_eq!(folders(&targets), ["_forever_"], "{context}");
        assert!(warnings.is_empty(), "{context}");
    }

    /// Each sign of a client, alone, keeps the folder a target: the predicate needs all
    /// four conditions, and evidence it cannot read is no evidence.
    #[test]
    fn any_sign_of_a_client_keeps_the_folder_a_target() {
        let listed = build_info(&[("wowf", "1.60.1.70790", true)]);
        let padded = {
            let mut text = listed.clone();
            let line = format!("## {}\n", "x".repeat(1_000));
            while text.len() as u64 <= crate::limits::DISCOVERY_FILE_MAX_BYTES {
                text.push_str(&line);
            }
            text
        };
        type Plant = fn(&Path);
        #[rustfmt::skip]
        let cases: &[(&str, Plant, Option<&str>, BattlenetHints)] = &[
            ("all_four_hold_the_control", |_| {}, None, BattlenetHints::default()),
            ("a_flavor_info", |r| flavor(r, "_classic_", "wow_classic"), None, BattlenetHints::default()),
            ("a_flavor_info_in_another_case", |r| write(r.join("_classic_").join(".Flavor.Info"), "x"), None, BattlenetHints::default()),
            ("an_executable", |r| write(r.join("_classic_").join("WowClassic.exe"), "MZ"), None, BattlenetHints::default()),
            ("another_wow_executable", |r| write(r.join("_classic_").join("WowB.exe"), "MZ"), None, BattlenetHints::default()),
            ("a_build_info_row_for_the_product", |_| {}, Some("row"), BattlenetHints::default()),
            ("a_build_info_without_a_product_column", |_| {}, Some("Branch!STRING:0|Active!DEC:1|Version!STRING:0\nus|1|1.60.1.70790\n"), BattlenetHints::default()),
            ("a_build_info_that_is_no_bpsv", |_| {}, Some("not bpsv at all"), BattlenetHints::default()),
            ("a_build_info_past_the_read_bound", |_| {}, Some("padded"), BattlenetHints::default()),
            ("a_build_info_that_is_a_directory", |r| fs::create_dir_all(r.join(".build.info")).unwrap(), Some("none"), BattlenetHints::default()),
            ("a_product_db_entry_for_the_folder", |_| {}, None, BattlenetHints {
                product_by_folder: BTreeMap::from([("_CLASSIC_".into(), "wow_classic".into())]),
                ..BattlenetHints::default()
            }),
            ("a_product_db_entry_for_the_product", |_| {}, None, BattlenetHints {
                version_by_product: BTreeMap::from([("wow_classic".into(), "5.5.4.63000".into())]),
                ..BattlenetHints::default()
            }),
        ];
        for (name, plant, text, hints) in cases {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path();
            match *text {
                None => write(root.join(".build.info"), &listed),
                Some("row") => write(
                    root.join(".build.info"),
                    &build_info(&[
                        ("wowf", "1.60.1.70790", true),
                        ("wow_classic", "5.5.4.63000", true),
                    ]),
                ),
                Some("padded") => write(root.join(".build.info"), &padded),
                Some("none") => {}
                Some(text) => write(root.join(".build.info"), text),
            }
            live(root, "_forever_", "wowf", "Wow.exe");
            ghost(root);
            plant(root);
            let (targets, warnings) = discover(root, hints);
            let context = format!("case {name}:\n{}", dump(&targets, &warnings));
            let want: &[&str] = if *name == "all_four_hold_the_control" {
                &["_forever_"]
            } else {
                &["_classic_", "_forever_"]
            };
            assert_eq!(folders(&targets), want, "{context}");
        }
    }

    #[test]
    fn versions_come_from_any_build_info_layout() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root.join(".build.info"),
            "Product!STRING:0|Active!DEC:1|Version!STRING:0\nwow|0|12.1.0.70000\nwow|1|12.1.5.70391\n",
        );
        flavor(root, "_retail_", "wow");
        let (targets, _) = discover(root, &BattlenetHints::default());
        assert_eq!(targets[0].target.interface, 120_105, "the active row wins");

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root.join(".build.info"),
            "Branch!STRING:0|Active!DEC:1|Version!STRING:0\nus|1|5.5.4.63000\n",
        );
        flavor(root, "_classic_", "wow_classic");
        let (targets, _) = discover(root, &BattlenetHints::default());
        assert_eq!(
            (targets[0].target.family, targets[0].target.interface),
            (Family::Mists, 50_504),
            "a single row without Product"
        );

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root.join(".build.info"), "not bpsv at all");
        fs::create_dir_all(root.join("_classic_era_")).unwrap();
        let hints = BattlenetHints {
            version_by_product: BTreeMap::from([("wow_classic_era".into(), "1.15.9".into())]),
            ..BattlenetHints::default()
        };
        let (targets, warnings) = discover(root, &hints);
        assert_eq!(
            (targets[0].target.family, targets[0].target.interface),
            (Family::Vanilla, 11_509),
            "{}",
            dump(&targets, &warnings)
        );
    }

    #[test]
    fn unknown_folders_are_usable_targets() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root.join(".build.info"),
            &build_info(&[("wowz", "13.0.0.80000", true)]),
        );
        flavor(root, "_xyz_", "wowz");
        write(root.join("_xyz_").join("WowZ.exe"), "MZ");
        let (targets, warnings) = discover(root, &BattlenetHints::default());
        let context = dump(&targets, &warnings);
        let t = &targets[0].target;
        assert_eq!(
            (
                t.id.as_str(),
                t.family,
                t.interface,
                t.kind,
                t.exe_name.as_deref()
            ),
            (
                "xyz",
                Family::Mainline,
                130_000,
                TargetKind::Live,
                Some("WowZ.exe")
            ),
            "{context}"
        );
        assert!(warnings.is_empty(), "{context}");

        fs::create_dir_all(root.join("_new_ptr_")).unwrap();
        let (targets, warnings) = discover(root, &BattlenetHints::default());
        let context = dump(&targets, &warnings);
        let t = &targets[0].target;
        assert_eq!(
            (
                t.id.as_str(),
                t.family,
                t.interface,
                t.kind,
                t.product.as_deref()
            ),
            ("new_ptr", Family::Unknown, 0, TargetKind::Test, None),
            "{context}"
        );
        assert_eq!(t.version, ClientVersion::new(0, 0, 0));
        assert_eq!(
            t.rules,
            *RulesTable::builtin().fallback(InstallKind::Battlenet)
        );
        // `unknown_flavor` is the World's, once overrides applied.
        assert!(warnings.is_empty(), "{context}");
        let unknown = super::super::unknown_flavor(t).unwrap();
        assert_eq!(unknown.code, WarningCode::UnknownFlavor);
        assert!(unknown.message.contains("_new_ptr_"), "{}", unknown.message);
        assert!(
            !unknown.message.contains(&*root.to_string_lossy()),
            "no full paths"
        );
    }

    #[test]
    fn an_unmapped_version_keeps_its_interface_and_warns() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root.join(".build.info"),
            &build_info(&[("wow_classic_era", "2.6.0.1", true)]),
        );
        flavor(root, "_classic_era_", "wow_classic_era");
        let (targets, warnings) = discover(root, &BattlenetHints::default());
        let t = &targets[0].target;
        assert_eq!(
            (t.family, t.interface),
            (Family::Unknown, 20_600),
            "{}",
            dump(&targets, &warnings)
        );
        let unknown = super::super::unknown_flavor(t).unwrap();
        assert!(
            unknown
                .message
                .contains("runs version 2.6.0.1, which no family covers"),
            "{}",
            unknown.message
        );
    }

    #[test]
    fn a_stale_last_addon_version_warns_and_keeps_the_computed_interface() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root.join(".build.info"),
            &build_info(&[("wow", "12.1.5.70391", true)]),
        );
        flavor(root, "_retail_", "wow");
        write(
            root.join("_retail_").join("WTF").join("Config.wtf"),
            "SET lastAddonVersion \"120100\"\n",
        );
        let (mut targets, warnings) = discover(root, &BattlenetHints::default());
        let context = dump(&targets, &warnings);
        assert_eq!(targets[0].target.interface, 120_105, "{context}");
        // Recorded on the target, and said once the registry has named it (omad-5cg5).
        assert!(warnings.is_empty(), "{context}");
        assert_eq!(targets[0].not_launched_since, Some(120_100), "{context}");
        let warning = super::super::not_launched_since_patch(&targets[0]).unwrap();
        assert_eq!(warning.code, WarningCode::ClientNotLaunchedSincePatch);
        assert_eq!(
            warning.fix.as_ref().map(Suggestion::display),
            Some("launch the client once")
        );
        assert_eq!(warning.fix.as_ref().and_then(Suggestion::argv), None);
        assert!(warning.message.starts_with("retail:"), "{context}");
        assert_eq!(warning.targets, ["retail"], "{context}");
        // Under the id the registry gave it, in the message and `targets` alike.
        targets[0].target.id = "main".to_owned();
        let renamed = super::super::not_launched_since_patch(&targets[0]).unwrap();
        assert!(renamed.message.starts_with("main:"), "{}", renamed.message);
        assert_eq!(renamed.targets, ["main"]);

        write(
            root.join("_retail_").join("WTF").join("Config.wtf"),
            "SET lastAddonVersion \"120105\"\n",
        );
        assert_eq!(
            discover(root, &BattlenetHints::default()).0[0].not_launched_since,
            None
        );
        // Newer than the version read: launching cannot clear that, so no warning.
        write(
            root.join("_retail_").join("WTF").join("Config.wtf"),
            "SET lastAddonVersion \"120107\"\n",
        );
        assert_eq!(
            discover(root, &BattlenetHints::default()).0[0].not_launched_since,
            None
        );
    }

    #[test]
    fn roots_are_recognized_by_build_info() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!is_battlenet_root(dir.path()));
        fs::create_dir_all(dir.path().join(".build.info")).unwrap();
        assert!(
            !is_battlenet_root(dir.path()),
            "a directory is not .build.info"
        );
        let dir = tempfile::tempdir().unwrap();
        write(dir.path().join(".build.info"), HEADER);
        assert!(is_battlenet_root(dir.path()));
        assert!(
            discover(&dir.path().join("missing"), &BattlenetHints::default())
                .0
                .is_empty()
        );
    }
}
