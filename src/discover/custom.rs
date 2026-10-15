//! Custom (private-server) clients: a root holding `Wow.exe` beside `Interface/` and no
//! `.build.info` (vision §4.2, §5.2 step 4). The version comes from the exe's
//! `VS_FIXEDFILEINFO`; family, interface and rules from the rules table's custom rows
//! (`-legacy` families leave the patch digit out of the interface). On legacy rows the
//! client's own `lastAddonVersion` overrides the computed interface. Reads only.

use std::path::Path;

use super::{DiscoveredTarget, DiscoverySource, config_wtf, find_child, pe, realmlist};
use crate::error::{Suggestion, Warning, WarningCode};
use crate::model::Target;
use crate::rules::{ClientVersion, Family, InstallKind, RulesTable, TargetKind, interface_for};

/// Whether `dir` is a custom client root: no `.build.info`, a client exe and an
/// `Interface/` directory (names ignore ASCII case), and not a Battle.net flavor folder,
/// which looks the same but whose parent holds `.build.info`.
pub fn is_custom_root(dir: &Path) -> bool {
    find_child(dir, ".build.info").is_none()
        && !dir
            .parent()
            .is_some_and(super::battlenet::is_battlenet_root)
        && client_exe(dir).is_some()
        && find_child(dir, "Interface").is_some_and(|p| p.is_dir())
}

/// The client exe's on-disk name: `wow*.exe` ignoring case, never `wowerror.exe`.
fn client_exe(dir: &Path) -> Option<String> {
    let names: Vec<String> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_file())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect();
    pick_exe(&names)
}

/// `wow.exe` first, then `wow-64.exe` (5.4.8 ships both), then the other `wow*.exe`
/// names sorted; all ignoring ASCII case.
pub(crate) fn pick_exe(names: &[String]) -> Option<String> {
    let mut candidates: Vec<&String> = names
        .iter()
        .filter(|n| {
            let lower = n.to_ascii_lowercase();
            lower.starts_with("wow") && lower.ends_with(".exe") && lower != "wowerror.exe"
        })
        .collect();
    let rank = |n: &str| match n.to_ascii_lowercase().as_str() {
        "wow.exe" => 0,
        "wow-64.exe" => 1,
        _ => 2,
    };
    candidates.sort_by(|a, b| (rank(a), a.as_str()).cmp(&(rank(b), b.as_str())));
    candidates.first().map(|n| (*n).clone())
}

/// The target of a custom root, or `None` when `root` is not one; plus `unknown_flavor`
/// when the exe has no version a family covers (the target stays usable).
pub fn discover_custom_root(
    root: &Path,
    table: &RulesTable,
    qualifier: &str,
    source: &DiscoverySource,
) -> (Option<DiscoveredTarget>, Vec<Warning>) {
    if !is_custom_root(root) {
        return (None, Vec::new());
    }
    let Some(exe) = client_exe(root) else {
        return (None, Vec::new());
    };
    let read = pe::exe_version(&root.join(&exe));
    let (version, family, rules) = match read {
        Some(v) => (
            v,
            table.family_for(&v, InstallKind::Custom),
            table.rules_for(&v, InstallKind::Custom).clone(),
        ),
        None => (
            ClientVersion::new(0, 0, 0),
            Family::Unknown,
            table.fallback(InstallKind::Custom).clone(),
        ),
    };
    let mut interface = if read.is_some() {
        interface_for(&version, &rules)
    } else {
        0
    };
    if rules.legacy
        && let Some(last) = config_wtf::last_addon_version(root)
    {
        interface = last;
    }
    let default_id = table
        .default_id_for_family(family)
        .map_or_else(|| id_from_dir(root), str::to_owned);

    let mut warnings = Vec::new();
    if family == Family::Unknown {
        let what = match read {
            Some(v) => format!("{exe} reports version {v}, which no family covers"),
            None => format!("{exe} has no readable version"),
        };
        warnings.push(
            Warning::new(
                WarningCode::UnknownFlavor,
                format!("{default_id}: {what}; it is usable with conservative legacy rules"),
            )
            .with_fix(Suggestion::text(
                "set its family with a [targets.<id>] override in config.toml",
            )),
        );
    }

    let label = realmlist::label_for(realmlist::realmlist_host(root).as_deref(), root);
    let target = Target {
        key: None,
        id: default_id.clone(),
        install_root: root.to_path_buf(),
        client_dir: root.to_path_buf(),
        flavor_dir: None,
        product: None,
        install_kind: InstallKind::Custom,
        family,
        kind: TargetKind::Live,
        version,
        interface,
        exe_name: Some(exe),
        label: Some(label),
        rules,
    };
    let discovered = DiscoveredTarget {
        target,
        default_id,
        qualifier: qualifier.to_owned(),
        source: source.clone(),
    };
    (Some(discovered), warnings)
}

/// An id from the root's directory name ([`super::slug`]); `custom` for `/`.
fn id_from_dir(root: &Path) -> String {
    root.file_name().map_or_else(
        || "custom".to_owned(),
        |n| super::slug(&n.to_string_lossy()),
    )
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|n| (*n).to_owned()).collect()
    }

    #[test]
    fn exe_preference() {
        for (listing, expected) in [
            (
                &["Wow-64.exe", "Wow.exe", "WowError.exe"][..],
                Some("Wow.exe"),
            ),
            (&["WOW-64.EXE", "WowB.exe"][..], Some("WOW-64.EXE")),
            (&["wowclassic.exe", "WowB.exe"][..], Some("WowB.exe")),
            (&["WOW.EXE"][..], Some("WOW.EXE")),
            (&["WowError.exe", "Launcher.exe", "wow.txt"][..], None),
            (&[][..], None),
        ] {
            assert_eq!(
                pick_exe(&names(listing)).as_deref(),
                expected,
                "{listing:?}"
            );
        }
    }

    #[test]
    fn ids_from_directory_names() {
        assert_eq!(
            id_from_dir(&PathBuf::from("/g/My Server_3.3.5")),
            "my-server-3-3-5"
        );
        assert_eq!(id_from_dir(&PathBuf::from("/g/wotlk")), "wotlk");
        assert_eq!(id_from_dir(Path::new("/")), "custom");
    }

    #[test]
    fn roots_need_an_exe_and_interface_and_no_build_info() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        assert!(!is_custom_root(root));
        fs::write(root.join("WowError.exe"), "").unwrap();
        fs::create_dir_all(root.join("interface")).unwrap();
        assert!(
            !is_custom_root(root),
            "the crash reporter alone is not a client"
        );
        fs::write(root.join("WOW.EXE"), "").unwrap();
        assert!(is_custom_root(root));
        fs::write(root.join(".build.info"), "Branch!STRING:0\n").unwrap();
        assert!(!is_custom_root(root), "a Battle.net root is never custom");
        fs::remove_file(root.join(".build.info")).unwrap();
        fs::remove_dir(root.join("interface")).unwrap();
        fs::write(root.join("Interface"), "a file").unwrap();
        assert!(!is_custom_root(root), "Interface must be a directory");
    }

    #[test]
    fn an_unreadable_exe_is_a_usable_unknown_target() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("Turtle WoW");
        fs::create_dir_all(root.join("Interface/AddOns")).unwrap();
        fs::write(root.join("WoW.exe"), "not a PE").unwrap();
        fs::create_dir_all(root.join("WTF")).unwrap();
        fs::write(
            root.join("WTF/Config.wtf"),
            "SET lastAddonVersion \"11200\"\n",
        )
        .unwrap();
        let (found, warnings) = discover_custom_root(
            &root,
            &RulesTable::builtin(),
            "custom",
            &DiscoverySource::Scan,
        );
        let t = found.unwrap().target;
        assert_eq!(
            (t.id.as_str(), t.family, t.interface, t.exe_name.as_deref()),
            ("turtle-wow", Family::Unknown, 11_200, Some("WoW.exe")),
            "the fallback row is legacy, so lastAddonVersion still sets the interface"
        );
        assert_eq!(t.label.as_deref(), Some("Turtle WoW"));
        assert_eq!(
            t.rules,
            *RulesTable::builtin().fallback(InstallKind::Custom)
        );
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].code, WarningCode::UnknownFlavor);
        assert!(
            warnings[0]
                .message
                .starts_with("turtle-wow: WoW.exe has no readable version")
        );
    }
}
