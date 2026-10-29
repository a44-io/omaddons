//! The realistic home (M4 plan §6.19, lifted from `tests/perf.rs`'s bench home): retail,
//! classic and era, each with the same 10 addons `gh:bench/addon00` … `addon09` at
//! release `v1`, two folders per addon (`AddonNN` and `AddonNN_Options`), each a TOC for
//! all three interfaces and 83 Lua files: 30 records and 5,040 managed files. It is
//! installed offline from local archives under `gh:` refs (release ids `@sha256:`), so it
//! builds in release builds, which replay no fixture. `bench_home` keeps its GitHub
//! fixtures for `argc bench`.

use std::fs;
use std::path::{Path, PathBuf};

use super::archives::zip_text;
use super::home::Home;

/// The three clients' target ids.
pub const REALISTIC_TARGETS: [&str; 3] = ["retail", "classic", "era"];
/// Their product codes.
const CLIENTS: [&str; 3] = ["wow", "wow_classic", "wow_classic_era"];
/// Their client folders.
const CLIENT_DIRS: [&str; 3] = ["_retail_", "_classic_", "_classic_era_"];
/// Addons on each client.
pub const ADDONS: usize = 10;
/// Lua files in each folder, besides its TOC.
pub const LUA_FILES: usize = 83;
/// Managed files over the three clients.
pub const MANAGED_FILES: usize = 5_040;

/// The realistic home, and where its archives are written.
pub struct Realistic {
    pub home: Home,
    archives: PathBuf,
}

/// Builds the realistic home at `dir` (which must not exist yet; it is kept, so give it
/// a temporary directory) with 30 offline installs, and asserts its shape.
pub fn realistic_home(dir: &Path) -> Realistic {
    assert!(!dir.exists(), "{} exists", dir.display());
    let home = Home::at(dir, &CLIENTS);
    let archives = dir.join("archives");
    fs::create_dir_all(&archives).unwrap();
    let realistic = Realistic { home, archives };
    for target in REALISTIC_TARGETS {
        for n in 0..ADDONS {
            let mut args = realistic.install_args(n, "v1", target);
            args.push("--json".to_owned());
            realistic.home.exec(&args).expect(0);
        }
    }
    let records: usize = REALISTIC_TARGETS
        .iter()
        .map(|t| super::state::record(&realistic.home, t).addons.len())
        .sum();
    assert_eq!(records, REALISTIC_TARGETS.len() * ADDONS, "records");
    let files: usize = CLIENT_DIRS
        .iter()
        .map(|c| count_files(&realistic.home.addons(c)))
        .sum();
    assert_eq!(files, MANAGED_FILES, "managed files under the three AddOns");
    realistic
}

impl Realistic {
    /// `<dir>/archives/addonNN-<tag>.zip`, written on first use: `AddonNN` and
    /// `AddonNN_Options`, each its TOC (`## Version: <tag>`) and [`LUA_FILES`] files.
    pub fn archive(&self, n: usize, tag: &str) -> PathBuf {
        let path = self.archives.join(format!("addon{n:02}-{tag}.zip"));
        if path.exists() {
            return path;
        }
        let name = format!("Addon{n:02}");
        let toc = format!("## Interface: 120105, 50504, 11509\n## Version: {tag}\n");
        let mut entries: Vec<(String, String)> = Vec::new();
        for folder in [name.clone(), format!("{name}_Options")] {
            entries.push((format!("{folder}/{folder}.toc"), toc.clone()));
            for i in 0..LUA_FILES {
                entries.push((format!("{folder}/f{i:03}.lua"), format!("-- {tag} {i}\n")));
            }
        }
        let borrowed: Vec<(&str, &str)> = entries
            .iter()
            .map(|(p, t)| (p.as_str(), t.as_str()))
            .collect();
        fs::write(&path, zip_text(&borrowed)).unwrap();
        path
    }

    /// `install gh:bench/addonNN --file <archive> -t <target> --offline`, without
    /// presentation flags: over an installed addon, an update of it.
    pub fn install_args(&self, n: usize, tag: &str, target: &str) -> Vec<String> {
        [
            "install",
            &format!("gh:bench/addon{n:02}"),
            "--file",
            &self.archive(n, tag).display().to_string(),
            "-t",
            target,
            "--offline",
        ]
        .iter()
        .map(|a| (*a).to_owned())
        .collect()
    }
}

/// Regular files under `dir`.
fn count_files(dir: &Path) -> usize {
    let mut count = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(at) = stack.pop() {
        for entry in fs::read_dir(&at).unwrap() {
            let entry = entry.unwrap();
            let kind = entry.file_type().unwrap();
            if kind.is_dir() {
                stack.push(entry.path());
            } else if kind.is_file() {
                count += 1;
            }
        }
    }
    count
}
