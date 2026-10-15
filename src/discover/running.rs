//! Which targets' clients are running right now (vision §5.2 step 8): computed live on
//! every call, never cached, never written.
//!
//! Wine names a process after its Windows exe (`/proc/<pid>/comm`, 15-byte limit). Only
//! the user's own processes whose `comm` matches a known exe name (case-insensitive,
//! truncation-aware, with or without `.exe`) have their `cmdline` and `environ` read:
//! the exe path in `cmdline` (a Windows path is mapped through the prefix's
//! `dosdevices`) names the client dir exactly; without it, `WINEPREFIX` narrows to the
//! targets with that exe in that prefix, all of which are reported running. A false
//! positive only makes the safe running policy refuse more; a false negative degrades
//! to a warning, never data loss, because transactions stay atomic. Only `WINEPREFIX`
//! is taken from `environ`, which can hold secrets.

use std::collections::BTreeMap;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};

use schemars::JsonSchema;
use serde::Serialize;

use super::dosdevices::map_windows_path;
use crate::model::Target;

/// `comm` holds at most this many bytes of the name.
const COMM_MAX: usize = 15;

/// One running client process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct RunningInfo {
    pub pid: u32,
    pub evidence: RunningEvidence,
}

/// How a process was tied to a target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RunningEvidence {
    /// The exe path in its command line is in the target's client dir.
    ExePath,
    /// Its exe name and Wine prefix match; the exe path could not be resolved.
    PrefixOnly,
}

/// Running clients by target `client_dir`, from the real `/proc`.
pub fn detect_running(targets: &[Target]) -> BTreeMap<PathBuf, Vec<RunningInfo>> {
    let Ok(uid) = std::fs::metadata("/proc/self").map(|m| m.uid()) else {
        return BTreeMap::new();
    };
    let home = std::env::var_os("HOME").map(PathBuf::from);
    detect_running_in(Path::new("/proc"), uid, targets, home.as_deref())
}

/// [`detect_running`] over any proc tree; `home` locates Wine's default prefix.
pub fn detect_running_in(
    proc_root: &Path,
    own_uid: u32,
    targets: &[Target],
    home: Option<&Path>,
) -> BTreeMap<PathBuf, Vec<RunningInfo>> {
    detect(proc_root, own_uid, targets, home, &|p| {
        std::fs::read(p).ok()
    })
}

/// The `WINEPREFIX` of every own process whose `comm` matches one of `names` (exe
/// names), for discovery hints.
pub fn running_prefixes(proc_root: &Path, own_uid: u32, names: &[String]) -> Vec<PathBuf> {
    let known = known_names(names.iter().map(String::as_str));
    let read = |p: &Path| std::fs::read(p).ok();
    let mut prefixes: Vec<PathBuf> = processes(proc_root, own_uid, &known, &read)
        .filter_map(|p| p.prefix)
        .collect();
    prefixes.sort();
    prefixes.dedup();
    prefixes
}

fn detect(
    proc_root: &Path,
    own_uid: u32,
    targets: &[Target],
    home: Option<&Path>,
    read: &dyn Fn(&Path) -> Option<Vec<u8>>,
) -> BTreeMap<PathBuf, Vec<RunningInfo>> {
    let known = known_names(targets.iter().filter_map(|t| t.exe_name.as_deref()));
    let mut running: BTreeMap<PathBuf, Vec<RunningInfo>> = BTreeMap::new();
    if known.is_empty() {
        return running;
    }
    for process in processes(proc_root, own_uid, &known, read) {
        let prefix = process
            .prefix
            .clone()
            .or_else(|| home.map(|h| h.join(".wine")).filter(|p| p.is_dir()));
        let exe_path = exe_path(&process.cmdline, &process.exe, prefix.as_deref());
        let matched: Vec<&Target> = match &exe_path {
            Some(path) => {
                let dir = path.parent().unwrap_or(path);
                targets
                    .iter()
                    .filter(|t| same_dir(&t.client_dir, dir))
                    .collect()
            }
            None => targets
                .iter()
                .filter(|t| {
                    t.exe_name
                        .as_deref()
                        .is_some_and(|e| e.eq_ignore_ascii_case(&process.exe))
                        && (prefix
                            .as_deref()
                            .is_some_and(|p| t.install_root.starts_with(p))
                            || process
                                .cmdline
                                .iter()
                                .any(|arg| arg.contains(&*t.client_dir.to_string_lossy())))
                })
                .collect(),
        };
        let evidence = if exe_path.is_some() {
            RunningEvidence::ExePath
        } else {
            RunningEvidence::PrefixOnly
        };
        for target in matched {
            running
                .entry(target.client_dir.clone())
                .or_default()
                .push(RunningInfo {
                    pid: process.pid,
                    evidence,
                });
        }
    }
    running
}

/// A process whose `comm` matched.
struct Process {
    pid: u32,
    /// The matched exe name, as the target spells it lowercased.
    exe: String,
    cmdline: Vec<String>,
    prefix: Option<PathBuf>,
}

/// Lowercased name forms (`wow.exe`, `wow`, 15-byte truncations) → the exe name.
fn known_names<'a>(names: impl Iterator<Item = &'a str>) -> BTreeMap<String, String> {
    let mut known = BTreeMap::new();
    for name in names {
        let lower = name.to_ascii_lowercase();
        let stem = lower.strip_suffix(".exe").unwrap_or(&lower).to_owned();
        for form in [lower.clone(), stem] {
            let truncated: String = form.bytes().take(COMM_MAX).map(char::from).collect();
            known.insert(truncated, lower.clone());
            known.insert(form, lower.clone());
        }
    }
    known
}

/// The user's own processes whose `comm` is a known name; `cmdline` and `environ` are
/// read for those only.
fn processes<'a>(
    proc_root: &Path,
    own_uid: u32,
    known: &'a BTreeMap<String, String>,
    read: &'a dyn Fn(&Path) -> Option<Vec<u8>>,
) -> impl Iterator<Item = Process> + 'a {
    let listing = std::fs::read_dir(proc_root)
        .into_iter()
        .flatten()
        .filter_map(Result::ok);
    listing.filter_map(move |entry| {
        let pid: u32 = entry.file_name().to_str()?.parse().ok()?;
        let dir = entry.path();
        if std::fs::symlink_metadata(&dir).ok()?.uid() != own_uid {
            return None;
        }
        let comm = read(&dir.join("comm"))?;
        let comm = String::from_utf8_lossy(&comm)
            .trim_end_matches('\n')
            .to_ascii_lowercase();
        let exe = known.get(&comm)?.clone();
        let cmdline = nul_separated(&read(&dir.join("cmdline")).unwrap_or_default());
        let prefix = nul_separated(&read(&dir.join("environ")).unwrap_or_default())
            .into_iter()
            .find_map(|var| var.strip_prefix("WINEPREFIX=").map(PathBuf::from))
            .filter(|p| p.is_absolute());
        Some(Process {
            pid,
            exe,
            cmdline,
            prefix,
        })
    })
}

fn nul_separated(bytes: &[u8]) -> Vec<String> {
    bytes
        .split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect()
}

/// The first argument naming `exe`, as a Linux path: Windows paths go through the
/// prefix's `dosdevices`.
fn exe_path(cmdline: &[String], exe: &str, prefix: Option<&Path>) -> Option<PathBuf> {
    cmdline.iter().find_map(|arg| {
        let base = arg.rsplit(['/', '\\']).next()?;
        if !base.eq_ignore_ascii_case(exe) {
            return None;
        }
        let windows = arg.contains('\\') || arg.as_bytes().get(1) == Some(&b':');
        if windows {
            map_windows_path(prefix?, arg)
        } else {
            Some(PathBuf::from(arg)).filter(|p| p.is_absolute())
        }
    })
}

/// The same directory: equal canonical paths, else equal components ignoring ASCII
/// case (a Wine path spelled differently).
fn same_dir(a: &Path, b: &Path) -> bool {
    if let (Ok(ca), Ok(cb)) = (std::fs::canonicalize(a), std::fs::canonicalize(b))
        // ubs:ignore — comparing paths, not secrets
        && ca == cb
    {
        return true;
    }
    let parts = |p: &Path| -> Vec<String> {
        p.components()
            .filter(|c| !matches!(c, Component::CurDir))
            .map(|c| c.as_os_str().to_string_lossy().to_ascii_lowercase())
            .collect()
    };
    parts(a) == parts(b)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn name_forms() {
        let known = known_names(["WowClassicT.exe", "WowClassicBeta.exe", "Wow.exe"].into_iter());
        for (comm, exe) in [
            ("wowclassict.exe", "wowclassict.exe"),
            ("wowclassicbeta.", "wowclassicbeta.exe"),
            ("wowclassicbeta", "wowclassicbeta.exe"),
            ("wow", "wow.exe"),
            ("wow.exe", "wow.exe"),
        ] {
            assert_eq!(known.get(comm).map(String::as_str), Some(exe), "{comm}");
        }
        assert!(!known.contains_key("wowclassic"));
    }

    #[test]
    fn environ_and_cmdline_only_for_matches() {
        let dir = tempfile::tempdir().unwrap();
        for (pid, comm) in [(10, "bash"), (11, "Wow.exe"), (12, "firefox")] {
            let p = dir.path().join(pid.to_string());
            std::fs::create_dir_all(&p).unwrap();
            std::fs::write(p.join("comm"), format!("{comm}\n")).unwrap();
            std::fs::write(p.join("cmdline"), b"x\0").unwrap();
            std::fs::write(p.join("environ"), b"A=B\0").unwrap();
        }
        let uid = std::fs::metadata(dir.path()).unwrap().uid();
        let seen = RefCell::new(BTreeSet::new());
        let read = |p: &Path| {
            let rel = p.strip_prefix(dir.path()).unwrap().display().to_string();
            seen.borrow_mut().insert(rel);
            std::fs::read(p).ok()
        };
        let mut target = crate::engine::testing::target(&dir.path().join("_retail_"));
        target.exe_name = Some("Wow.exe".into());
        let _ = detect(dir.path(), uid, &[target], None, &read);
        let seen = seen.into_inner();
        assert!(
            seen.contains("11/environ") && seen.contains("11/cmdline"),
            "{seen:?}"
        );
        for pid in ["10", "12"] {
            assert!(
                !seen.contains(&format!("{pid}/environ"))
                    && !seen.contains(&format!("{pid}/cmdline")),
                "{seen:?}"
            );
            assert!(seen.contains(&format!("{pid}/comm")));
        }
    }

    #[test]
    fn same_dir_ignores_case_and_dots() {
        assert!(same_dir(
            Path::new("/x/Program Files/World of Warcraft/_retail_"),
            Path::new("/x/program files/./world of warcraft/_RETAIL_")
        ));
        assert!(!same_dir(
            Path::new("/x/_retail_"),
            Path::new("/x/_classic_")
        ));
    }
}
