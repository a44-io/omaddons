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
use crate::error::{Suggestion, Warning, WarningCode};
use crate::model::Target;
use crate::rules::{ClientVersion, Family, InstallKind, RulesTable, TargetKind, interface_for};

/// What `product.db` knows about a root, passed in by the scan; empty without hints.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
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

/// Every flavor folder of `root` as a target, sorted by folder name, plus warnings
/// (`unknown_flavor`, `client_not_launched_since_patch`). Reads only.
pub fn discover_root(
    root: &Path,
    table: &RulesTable,
    hints: &BattlenetHints,
    qualifier: &str,
    source: &DiscoverySource,
) -> (Vec<DiscoveredTarget>, Vec<Warning>) {
    let rows = read_text(&root.join(".build.info"))
        .and_then(|text| bpsv::build_info_rows(&text).ok())
        .unwrap_or_default();
    let mut targets = Vec::new();
    let mut warnings = Vec::new();
    for folder in flavor_folders(root) {
        let client_dir = root.join(&folder);
        let product = read_text(&client_dir.join(".flavor.info"))
            .and_then(|text| bpsv::flavor_product(&text))
            .or_else(|| hints.product_by_folder.get(&folder).cloned())
            .or_else(|| table.product_by_folder(&folder).map(|p| p.code.clone()));
        let known = product.as_deref().and_then(|p| table.product_by_code(p));
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
        let interface = if family == Family::Unknown && version == ClientVersion::new(0, 0, 0) {
            0
        } else {
            interface_for(&version, &rules)
        };
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
        let exe_name = exe_name(&client_dir, known.map(|p| p.exe.as_str()));

        if family == Family::Unknown {
            let what = if version == ClientVersion::new(0, 0, 0) {
                "has no readable version".to_owned()
            } else {
                format!("runs version {version}, which no family covers")
            };
            warnings.push(
                Warning::new(
                    WarningCode::UnknownFlavor,
                    format!(
                        "{default_id}: flavor folder {folder} {what}; it is usable with permissive modern rules"
                    ),
                )
                .with_fix(Suggestion::text(
                    "set its family with a [targets.<id>] override in config.toml",
                )),
            );
        }
        if let Some(last) = config_wtf::last_addon_version(&client_dir)
            && interface != 0
            && last != interface
        {
            warnings.push(
                Warning::new(
                    WarningCode::ClientNotLaunchedSincePatch,
                    format!(
                        "{default_id}: the client last loaded addons at interface {last}, but version {version} is interface {interface}"
                    ),
                )
                .with_fix(Suggestion::text("launch the client once")),
            );
        }

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
                rules,
            },
            default_id,
            qualifier: qualifier.to_owned(),
            source: source.clone(),
        });
    }
    (targets, warnings)
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
        .filter(|name| name.len() > 2 && name.starts_with('_') && name.ends_with('_'))
        .collect();
    folders.sort();
    folders
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
        assert_eq!(warnings.len(), 1, "{context}");
        assert_eq!(warnings[0].code, WarningCode::UnknownFlavor);
        assert!(warnings[0].message.contains("_new_ptr_"), "{context}");
        assert!(
            !warnings[0].message.contains(&*root.to_string_lossy()),
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
        assert_eq!(warnings[0].code, WarningCode::UnknownFlavor);
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
        let (targets, warnings) = discover(root, &BattlenetHints::default());
        let context = dump(&targets, &warnings);
        assert_eq!(targets[0].target.interface, 120_105, "{context}");
        assert_eq!(warnings.len(), 1, "{context}");
        assert_eq!(warnings[0].code, WarningCode::ClientNotLaunchedSincePatch);
        assert_eq!(
            warnings[0].fix.as_ref().map(Suggestion::display),
            Some("launch the client once")
        );
        assert_eq!(warnings[0].fix.as_ref().and_then(Suggestion::argv), None);
        assert!(warnings[0].message.starts_with("retail:"), "{context}");

        write(
            root.join("_retail_").join("WTF").join("Config.wtf"),
            "SET lastAddonVersion \"120105\"\n",
        );
        assert!(discover(root, &BattlenetHints::default()).1.is_empty());
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
