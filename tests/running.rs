//! Running-client detection against fake `/proc` trees and real-shaped targets.

mod common;

use std::collections::BTreeMap;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use common::env::TestEnv;
use common::procfs::{FakeProc, running_as_root};
use common::trees::{BattlenetRoot, CustomRoot, Prefix};
use omaddons::discover::DiscoverySource;
use omaddons::discover::battlenet::{BattlenetHints, discover_root};
use omaddons::discover::custom::discover_custom_root;
use omaddons::discover::running::{
    RunningEvidence, RunningInfo, detect_running_in, running_prefixes,
};
use omaddons::model::Target;
use omaddons::rules::RulesTable;
use support::{battlenet_targets, uid};

#[cfg(test)]
mod support {
    use super::*;

    /// Every target of a Battle.net root inside a Wine prefix.
    pub fn battlenet_targets(prefix: &Prefix, products: &[&str]) -> Vec<Target> {
        let mut root = BattlenetRoot::at(prefix.wow_root());
        for code in products {
            root = root.product(code);
        }
        let root = root.build();
        let (targets, _) = discover_root(
            &root,
            &RulesTable::builtin(),
            &BattlenetHints::default(),
            "battlenet",
            &DiscoverySource::Scan,
        );
        targets.into_iter().map(|t| t.target).collect()
    }

    /// The uid owning the fake proc tree (the test user).
    pub fn uid(proc: &FakeProc) -> u32 {
        std::fs::metadata(proc.root()).unwrap().uid()
    }
}

fn running(
    result: &BTreeMap<PathBuf, Vec<RunningInfo>>,
    client_dir: &Path,
) -> Vec<(u32, RunningEvidence)> {
    result
        .get(client_dir)
        .map(|infos| infos.iter().map(|i| (i.pid, i.evidence)).collect())
        .unwrap_or_default()
}

#[test]
fn a_windows_exe_path_maps_through_dosdevices() {
    let env = TestEnv::new();
    let prefix = Prefix::new(&env, "battlenet");
    let targets = battlenet_targets(&prefix, &["wow", "wow_classic"]);
    let retail = targets
        .iter()
        .find(|t| t.flavor_dir.as_deref() == Some("_retail_"))
        .unwrap();
    let classic = targets
        .iter()
        .find(|t| t.flavor_dir.as_deref() == Some("_classic_"))
        .unwrap();
    let prefix_path = prefix.path().display().to_string();
    let proc = FakeProc::new(env.root().join("proc"))
        .process(
            4242,
            true,
            "Wow.exe",
            &["C:\\Program Files (x86)\\World of Warcraft\\_retail_\\Wow.exe"],
            &[("WINEPREFIX", &prefix_path), ("SECRET", "x")],
        )
        .process(4243, true, "bash", &["bash"], &[]);
    let result = detect_running_in(proc.root(), uid(&proc), &targets, None);
    assert_eq!(
        running(&result, &retail.client_dir),
        [(4242, RunningEvidence::ExePath)],
        "{result:?}"
    );
    assert!(running(&result, &classic.client_dir).is_empty());
    assert_eq!(result.len(), 1);
    assert_eq!(
        running_prefixes(proc.root(), uid(&proc), &["Wow.exe".into()]),
        [prefix.path().to_path_buf()]
    );
}

#[test]
fn comm_names_match_case_insensitively_and_truncated() {
    let env = TestEnv::new();
    let prefix = Prefix::new(&env, "battlenet");
    let targets = battlenet_targets(&prefix, &["wow_classic_ptr"]);
    let ptr = &targets[0];
    assert_eq!(
        ptr.exe_name.as_deref().map(str::len),
        Some(15),
        "{:?}",
        ptr.exe_name
    );
    let prefix_path = prefix.path().display().to_string();
    let exe = format!(
        "C:\\Program Files (x86)\\World of Warcraft\\_classic_ptr_\\{}",
        ptr.exe_name.as_deref().unwrap()
    );
    let proc = FakeProc::new(env.root().join("proc")).process(
        7,
        true,
        &ptr.exe_name.as_deref().unwrap().to_uppercase(),
        &[&exe],
        &[("WINEPREFIX", &prefix_path)],
    );
    let result = detect_running_in(proc.root(), uid(&proc), &targets, None);
    assert_eq!(
        running(&result, &ptr.client_dir),
        [(7, RunningEvidence::ExePath)]
    );
}

#[test]
fn other_users_processes_are_ignored() {
    let env = TestEnv::new();
    let prefix = Prefix::new(&env, "battlenet");
    let targets = battlenet_targets(&prefix, &["wow"]);
    let prefix_path = prefix.path().display().to_string();
    let cmd = ["C:\\Program Files (x86)\\World of Warcraft\\_retail_\\Wow.exe"];
    let proc = FakeProc::new(env.root().join("proc")).process(
        1,
        true,
        "Wow.exe",
        &cmd,
        &[("WINEPREFIX", &prefix_path)],
    );
    assert!(
        detect_running_in(proc.root(), uid(&proc) + 1, &targets, None).is_empty(),
        "another uid owns every pid dir"
    );
    if !running_as_root() {
        let proc = FakeProc::new(env.root().join("proc2")).process(
            2,
            false,
            "Wow.exe",
            &cmd,
            &[("WINEPREFIX", &prefix_path)],
        );
        assert!(
            detect_running_in(proc.root(), uid(&proc), &targets, None).is_empty(),
            "unreadable files are skipped"
        );
    }
}

#[test]
fn an_unresolved_exe_marks_every_matching_target_in_the_prefix() {
    let env = TestEnv::new();
    let prefix = Prefix::new(&env, "battlenet");
    let targets = battlenet_targets(
        &prefix,
        &["wow", "wow_classic", "wow_classic_era", "wow_anniversary"],
    );
    let shared: Vec<&Target> = targets
        .iter()
        .filter(|t| {
            t.exe_name
                .as_deref()
                .is_some_and(|e| e.eq_ignore_ascii_case("WowClassic.exe"))
        })
        .collect();
    assert_eq!(
        shared.len(),
        3,
        "{:?}",
        targets
            .iter()
            .map(|t| (&t.flavor_dir, &t.exe_name))
            .collect::<Vec<_>>()
    );
    let prefix_path = prefix.path().display().to_string();
    let proc = FakeProc::new(env.root().join("proc")).process(
        55,
        true,
        "WowClassic.exe",
        &["WowClassic.exe"],
        &[("WINEPREFIX", &prefix_path)],
    );
    let result = detect_running_in(proc.root(), uid(&proc), &targets, None);
    for target in shared {
        assert_eq!(
            running(&result, &target.client_dir),
            [(55, RunningEvidence::PrefixOnly)],
            "{:?}",
            target.flavor_dir
        );
    }
    assert_eq!(result.len(), 3, "retail (Wow.exe) is not running");
}

#[test]
fn a_custom_client_outside_any_prefix() {
    let env = TestEnv::new();
    let root = CustomRoot::at(env.home().join("Games/wow-335"))
        .version(3, 3, 5, 12_340)
        .build();
    let (found, _) = discover_custom_root(
        &root,
        &RulesTable::builtin(),
        "custom",
        &DiscoverySource::Scan,
    );
    let target = found.unwrap().target;
    let exe = root
        .join(target.exe_name.as_deref().unwrap())
        .display()
        .to_string();
    let proc = FakeProc::new(env.root().join("proc"))
        .process(9, true, "Wow.exe", &["/usr/bin/wine", &exe], &[])
        .process(10, true, "Wow.exe", &["C:\\Games\\Wow.exe"], &[]);
    let result = detect_running_in(proc.root(), uid(&proc), std::slice::from_ref(&target), None);
    assert_eq!(
        running(&result, &target.client_dir),
        [(9, RunningEvidence::ExePath)],
        "pid 10 has no prefix to map its Windows path"
    );
}

#[test]
fn exited_and_odd_processes_are_skipped() {
    let env = TestEnv::new();
    let prefix = Prefix::new(&env, "battlenet");
    let targets = battlenet_targets(&prefix, &["wow"]);
    let proc = FakeProc::new(env.root().join("proc")).process(3, true, "Wow.exe", &[], &[]);
    std::fs::create_dir_all(proc.root().join("4")).unwrap();
    std::fs::create_dir_all(proc.root().join("self")).unwrap();
    let result = detect_running_in(proc.root(), uid(&proc), &targets, None);
    assert!(
        result.is_empty(),
        "no prefix and no path: nothing to tie it to\n{result:?}"
    );

    let home = env.home();
    std::fs::create_dir_all(home.join(".wine")).unwrap();
    let in_default = Prefix::at(home.join(".wine"));
    let default_targets = battlenet_targets(&in_default, &["wow"]);
    let proc = FakeProc::new(env.root().join("proc2")).process(
        5,
        true,
        "Wow.exe",
        &["C:\\Program Files (x86)\\World of Warcraft\\_retail_\\Wow.exe"],
        &[],
    );
    let result = detect_running_in(proc.root(), uid(&proc), &default_targets, Some(&home));
    assert_eq!(
        running(&result, &default_targets[0].client_dir),
        [(5, RunningEvidence::ExePath)],
        "~/.wine is the default prefix"
    );
}

/// The process shapes recorded on the development machine on 2026-10-20 (umu-launcher,
/// GE-Proton, pressure-vessel; one client at a time): umu points `WINEPREFIX` at
/// `<prefix>/pfx/`, a symlink to the prefix; `comm` is the exe's own spelling; the
/// voice proxy's name is truncated; the Battle.net app runs as `CrBrowserMain`; Wine
/// pads `cmdline` with empty arguments. Only the played client's target runs.
#[test]
fn observed_proton_shapes_map_to_the_played_client() {
    let env = TestEnv::new();
    let battlenet = Prefix::new(&env, "battlenet");
    let wow335 = Prefix::new(&env, "wow-335");
    for prefix in [&battlenet, &wow335] {
        std::os::unix::fs::symlink(".", prefix.path().join("pfx")).unwrap();
    }
    let mut targets = battlenet_targets(
        &battlenet,
        &["wow", "wow_classic", "wow_classic_era", "wow_anniversary"],
    );
    let root = CustomRoot::at(wow335.drive_c().join("WoW"))
        .version(3, 3, 5, 12_340)
        .build();
    let (found, _) = discover_custom_root(
        &root,
        &RulesTable::builtin(),
        "custom",
        &DiscoverySource::Scan,
    );
    targets.push(found.unwrap().target);
    let dir_of = |flavor: Option<&str>| {
        targets
            .iter()
            .find(|t| t.flavor_dir.as_deref() == flavor)
            .unwrap()
            .client_dir
            .clone()
    };

    let home = env.home().display().to_string();
    let bnet = battlenet.path().display().to_string();
    let bnet_pfx = format!("{bnet}/pfx/");
    let launcher = format!("{bnet}/drive_c/Program Files (x86)/Battle.net/Battle.net Launcher.exe");
    let shim = format!("{home}/.local/share/umu/umu-shim");
    let proton =
        format!("{home}/.local/share/Steam/compatibilitytools.d/GE-Proton11-11-x86_64/proton");
    let wineserver = format!(
        "{home}/.local/share/Steam/compatibilitytools.d/GE-Proton11-11-x86_64/files/bin/wineserver"
    );
    let pv_adverb = "/usr/lib/pressure-vessel/from-host/libexec/steam-runtime-tools-0/pv-adverb";
    let pad = [""; 8];
    let padded =
        |args: [&'static str; 2]| -> Vec<&'static str> { args.into_iter().chain(pad).collect() };
    let umu_exe: Vec<&str> = ["c:\\windows\\system32\\umu.exe", launcher.as_str()]
        .into_iter()
        .chain(pad)
        .collect();
    let browser = padded([
        "C:\\Program Files (x86)\\Battle.net\\Battle.net.exe",
        "--from-launcher",
    ]);
    let agent = padded([
        "C:/ProgramData/Battle.net/Agent/Agent.9824/Agent.exe",
        "--session=1",
    ]);
    let wow = "C:\\Program Files (x86)\\World of Warcraft";
    let retail = format!("{wow}\\_retail_\\WoW.exe");
    let mists = format!("{wow}\\_classic_\\WoWClassic.exe");
    let era = format!("{wow}\\_classic_era_\\WoWClassic.exe");
    let anniversary = format!("{wow}\\_anniversary_\\WoWClassic.exe");
    let sessions: [(&str, &str, Vec<&str>, PathBuf); 4] = [
        (
            "retail",
            "WoW.exe",
            vec![retail.as_str(), "-launcherlogin", "-uid", "wow"],
            dir_of(Some("_retail_")),
        ),
        // Only argv[0] was recorded for Mists Classic.
        (
            "classic",
            "WoWClassic.exe",
            vec![mists.as_str()],
            dir_of(Some("_classic_")),
        ),
        (
            "era",
            "WoWClassic.exe",
            vec![
                era.as_str(),
                "-launcherlogin",
                "-uid",
                "wow_classic_era",
                "-initialgamemode=cera",
            ],
            dir_of(Some("_classic_era_")),
        ),
        (
            "anniversary",
            "WoWClassic.exe",
            vec![
                anniversary.as_str(),
                "-launcherlogin",
                "-initialgamemode=bccfresh",
                "-uid",
                "wow_classic_anniversary",
            ],
            dir_of(Some("_anniversary_")),
        ),
    ];
    let names: Vec<String> = targets
        .iter()
        .filter_map(|t| t.exe_name.clone())
        .chain(["Wow.exe", "Wow-64.exe", "Battle.net.exe"].map(str::to_owned))
        .collect();
    for (id, comm, argv, client_dir) in sessions {
        let env_bnet = [("WINEPREFIX", bnet_pfx.as_str())];
        let proc = FakeProc::new(env.root().join(format!("proc-{id}")))
            .process(
                11,
                true,
                "umu-run",
                &["/usr/bin/python3", "/usr/bin/umu-run", &launcher],
                &[("WINEPREFIX", &bnet)],
            )
            .process(
                12,
                true,
                "pv-adverb",
                &[pv_adverb, "--", &shim, &proton, "run", &launcher],
                &env_bnet,
            )
            .process(13, true, "umu.exe", &umu_exe, &env_bnet)
            .process(14, true, "wineserver", &[&wineserver], &env_bnet)
            .process(15, true, "CrBrowserMain", &browser, &env_bnet)
            .process(16, true, "Agent.exe", &agent, &env_bnet)
            .process(20, true, comm, &argv, &env_bnet)
            .process(
                21,
                true,
                "WowVoiceProxy.e",
                &["./Utils/WowVoiceProxy.exe"],
                &env_bnet,
            );
        let result = detect_running_in(proc.root(), uid(&proc), &targets, None);
        assert_eq!(
            result.into_iter().collect::<Vec<_>>(),
            [(
                client_dir,
                vec![RunningInfo {
                    pid: 20,
                    evidence: RunningEvidence::ExePath
                }]
            )],
            "{id}"
        );
        assert_eq!(
            running_prefixes(proc.root(), uid(&proc), &names),
            [std::fs::canonicalize(battlenet.path()).unwrap()],
            "{id}: the pfx symlink names the prefix itself"
        );
    }

    // 3.3.5a: its own prefix, also through umu, `Wow.exe` from `C:\WoW`.
    let w335 = wow335.path().display().to_string();
    let w335_exe = format!("{w335}/drive_c/WoW/Wow.exe");
    let w335_pfx = format!("{w335}/pfx/");
    let env_335 = [("WINEPREFIX", w335_pfx.as_str())];
    let proc = FakeProc::new(env.root().join("proc-wotlk"))
        .process(
            31,
            true,
            "umu-run",
            &["/usr/bin/python3", "/usr/bin/umu-run", &w335_exe],
            &[("WINEPREFIX", &w335)],
        )
        .process(
            32,
            true,
            "pv-adverb",
            &[pv_adverb, "--", &shim, &proton, "run", &w335_exe],
            &env_335,
        )
        .process(33, true, "Wow.exe", &["C:\\WoW\\Wow.exe"], &env_335);
    let result = detect_running_in(proc.root(), uid(&proc), &targets, None);
    assert_eq!(
        result.into_iter().collect::<Vec<_>>(),
        [(
            dir_of(None),
            vec![RunningInfo {
                pid: 33,
                evidence: RunningEvidence::ExePath
            }]
        )]
    );
    assert_eq!(
        running_prefixes(proc.root(), uid(&proc), &names),
        [std::fs::canonicalize(wow335.path()).unwrap()]
    );
}
