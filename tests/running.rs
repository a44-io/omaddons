//! Running-client detection against fake `/proc` trees and real-shaped targets.

mod common;

use std::collections::BTreeMap;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::Instant;

use common::env::TestEnv;
use common::procfs::{FakeProc, running_as_root};
use common::trees::{BattlenetRoot, CustomRoot, Prefix};
use omaddons::discover::DiscoverySource;
use omaddons::discover::battlenet::{BattlenetHints, discover_root};
use omaddons::discover::custom::discover_custom_root;
use omaddons::discover::running::{
    RunningEvidence, RunningInfo, detect_running, detect_running_in, running_prefixes,
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

#[test]
#[ignore = "measures the real /proc; run at checkpoints"]
fn real_proc_within_budget() {
    let env = TestEnv::new();
    let prefix = Prefix::new(&env, "battlenet");
    let targets = battlenet_targets(&prefix, &["wow", "wow_classic", "wow_classic_era", "wowf"]);
    let _warm = detect_running(&targets);
    let started = Instant::now();
    let result = detect_running(&targets);
    let elapsed = started.elapsed();
    assert!(
        result.clients.is_empty(),
        "the synthetic targets are not running"
    );
    assert_eq!(
        result.unavailable, None,
        "the real /proc lists this process"
    );
    assert!(
        elapsed.as_micros() < 2_000,
        "detect_running took {elapsed:?}"
    );
}
