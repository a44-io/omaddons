//! An older build against the state this build writes (E9, checkpoint criterion 16,
//! M4-D58): the last M3 commit built with the toolchain it records ([`old_build`], [`m3`])
//! and run in a test home through T1's runner ([`OldRunner`]); two hours passing for it
//! ([`age`]); what M4 wrote and still needs ([`M4Inventory`]), the addon and `WTF` trees
//! that holds ([`FolderBytes`]) and history ([`history_prefix_kept`]), compared before and
//! after the old build's command; and the table README quotes ([`Row`], [`table`]).
//!
//! Never a skip: an old build that cannot be had leaves criterion 16 unmet, and [`m3`]
//! fails the test that asked for it. Nothing here uses the checkpoint's skip, or reads
//! the variable that accepts one (`common_selftest`'s `compat_suite_never_skips`).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write as _;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::env::TestEnv;
use super::home::Home;
use super::tlog::Runner;
use super::trees::tree_hash;

/// The last M3 commit.
pub const M3_COMMIT: &str = "c0ab5e904db26e95cade7fe1ef5129c27bb16ead";
pub const M3_SHORT: &str = "c0ab5e9";
/// Past either build's `ORPHAN_MIN_AGE` (one hour).
pub const AGE: Duration = Duration::from_secs(2 * 3600);
/// The build log's lines quoted when no binary can be had.
pub const BUILD_LOG_TAIL: usize = 40;

// ---------------------------------------------------------------------------------------
// The build.

/// An old commit's release binary and where it came from.
#[derive(Debug, Clone)]
pub struct OldBuild {
    pub short: String,
    pub bin: PathBuf,
    pub provenance: Provenance,
    /// Whether the binary is an earlier build's, verified against its record, because
    /// this build could not run.
    pub reused: bool,
}

/// `target/compat/<short>.provenance.json`, written right after a build from that commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    /// The full hash.
    pub commit: String,
    /// The channel read from the commit's `rust-toolchain.toml`.
    pub toolchain: String,
    /// `rustup run <channel> rustc -vV`, verbatim.
    pub rustc: String,
    pub profile: String,
    pub locked: bool,
    pub binary_sha256: String,
    /// RFC 3339, UTC.
    pub built_at: String,
}

/// The `[toolchain] channel` of a `rust-toolchain.toml`; `None` for a file that is not
/// TOML, has no `[toolchain]` table, or names a toolchain by path.
pub fn toolchain_channel(rust_toolchain_toml: &str) -> Option<String> {
    let parsed: toml::Table = rust_toolchain_toml.parse().ok()?;
    let channel = parsed
        .get("toolchain")?
        .as_table()?
        .get("channel")?
        .as_str()?;
    (!channel.is_empty()).then(|| channel.to_owned())
}

/// The lowercase hex sha256 of a file's bytes.
pub fn sha256_file(path: &Path) -> Result<String, String> {
    let bytes = fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(hex(&Sha256::digest(&bytes)))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Whether `record` vouches for a binary whose sha256 is `bin_sha256`, built from `full`
/// in a worktree whose `HEAD` is `worktree_head`.
pub fn verify(
    record: &Provenance,
    full: &str,
    worktree_head: &str,
    bin_sha256: &str,
) -> Result<(), String> {
    // ubs:ignore — comparing commit hashes and file digests for provenance, not secrets
    if record.commit != full {
        return Err(format!(
            "the record names commit {}, not {full}",
            record.commit
        ));
    }
    // ubs:ignore — comparing commit hashes, not secrets
    if worktree_head != full {
        return Err(format!(
            "the worktree's HEAD is {worktree_head}, not {full}"
        ));
    }
    // ubs:ignore — comparing file digests for provenance, not secrets
    if record.binary_sha256 != bin_sha256 {
        return Err(format!(
            "the binary's sha256 is {bin_sha256}, the record's {}",
            record.binary_sha256
        ));
    }
    Ok(())
}

/// The record at `record_path`, when it vouches for the binary at `bin` (see [`verify`]).
pub fn verified(
    record_path: &Path,
    bin: &Path,
    full: &str,
    worktree_head: &str,
) -> Result<Provenance, String> {
    let text = fs::read_to_string(record_path)
        .map_err(|e| format!("no provenance record at {}: {e}", record_path.display()))?;
    let record: Provenance = serde_json::from_str(&text)
        .map_err(|e| format!("{} does not parse: {e}", record_path.display()))?;
    verify(&record, full, worktree_head, &sha256_file(bin)?)?;
    Ok(record)
}

/// The repository root.
fn repo() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// `git <args>` in the repository; its trimmed stdout.
fn git(args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .current_dir(repo())
        .args(args)
        .output()
        .map_err(|e| format!("git {}: {e}", args.join(" ")))?;
    if !out.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// The detached worktree `target/compat/<short>` at `full`: made when missing, checked
/// when present, never deleted or reset (it may hold someone's build).
fn worktree(short: &str, full: &str) -> Result<PathBuf, String> {
    let dir = repo().join("target/compat").join(short);
    let at = dir.display().to_string();
    if !dir.exists() {
        // Forgets only worktrees whose directory is gone (which `cargo clean` leaves).
        git(&["worktree", "prune"])?;
        git(&["worktree", "add", "--detach", &at, full])?;
    }
    let head = git(&["-C", &at, "rev-parse", "HEAD"])?;
    // ubs:ignore — comparing commit hashes, not secrets
    if head != full {
        return Err(format!("target/compat/{short} holds {head}, not {full}"));
    }
    Ok(dir)
}

/// `rustup run <channel> rustc -vV`, installing the channel once when it is missing.
fn rustc_of(channel: &str, log: &Path) -> Result<String, String> {
    let version = || {
        Command::new("rustup")
            .args(["run", channel, "rustc", "-vV"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
    };
    if let Some(text) = version() {
        return Ok(text);
    }
    let installed = Command::new("rustup")
        .args(["toolchain", "install", channel, "--profile", "minimal"])
        .stdout(appending(log)?)
        .stderr(appending(log)?)
        .status()
        .is_ok_and(|s| s.success());
    match (installed, version()) {
        (true, Some(text)) => Ok(text),
        _ => Err(format!("toolchain {channel} could not be installed")),
    }
}

fn appending(log: &Path) -> Result<Stdio, String> {
    fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
        .map(Stdio::from)
        .map_err(|e| format!("{}: {e}", log.display()))
}

/// `cargo build --release --locked` in `worktree`, with its own toolchain and target
/// directory whatever the outer `cargo test` runs under; output appended to `log`.
fn build(worktree: &Path, channel: &str, log: &Path) -> Result<(), String> {
    let built = Command::new("cargo")
        .current_dir(worktree)
        .args(["build", "--release", "--locked"])
        .env("RUSTUP_TOOLCHAIN", channel)
        .env("CARGO_TARGET_DIR", worktree.join("target"))
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("CARGO_BUILD_TARGET")
        // The outer cargo's compiler would win over the channel.
        .env_remove("RUSTC")
        .env_remove("RUSTDOC")
        .stdout(appending(log)?)
        .stderr(appending(log)?)
        .status()
        .map_err(|e| format!("cargo build: {e}"))?;
    if built.success() {
        Ok(())
    } else {
        Err(format!("cargo build --release --locked failed ({built})"))
    }
}

fn tail(log: &Path, lines: usize) -> String {
    let text = fs::read_to_string(log).unwrap_or_default();
    let all: Vec<&str> = text.lines().collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}

/// The release binary of commit `full` (`short`), built with the toolchain the commit
/// records, or an earlier build of it that its provenance record vouches for. Never a
/// skip: `Err` says why neither could be had, with the build log's tail.
pub fn old_build(short: &str, full: &str) -> Result<OldBuild, String> {
    let compat = repo().join("target/compat");
    fs::create_dir_all(&compat).map_err(|e| format!("{}: {e}", compat.display()))?;
    // Two test processes never build at once; the lock goes with the file.
    let lock_path = compat.join(format!(".{short}.lock"));
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .map_err(|e| format!("{}: {e}", lock_path.display()))?;
    lock.lock()
        .map_err(|e| format!("{}: {e}", lock_path.display()))?;

    let log = compat.join(format!("{short}-build.log"));
    let record = compat.join(format!("{short}.provenance.json"));
    let unmet = |channel: &str, why: String| {
        format!(
            "{short} could not be built with its recorded toolchain ({channel}) and no \
             provenance-verified binary of it exists ({why}). This is a failure, never a SKIP; \
             file a bug against the M3 compatibility characterization. Last lines of \
             target/compat/{short}-build.log:\n{}",
            tail(&log, BUILD_LOG_TAIL)
        )
    };
    let dir = worktree(short, full).map_err(|why| unmet("unread", why))?;
    let bin = dir.join("target/release/omaddons");
    let toml = fs::read_to_string(dir.join("rust-toolchain.toml")).unwrap_or_default();
    let channel = toolchain_channel(&toml).ok_or_else(|| {
        unmet(
            "unread",
            format!("target/compat/{short}/rust-toolchain.toml names no channel"),
        )
    })?;

    let built =
        rustc_of(&channel, &log).and_then(|rustc| build(&dir, &channel, &log).map(|()| rustc));
    match built {
        Ok(rustc) => {
            let provenance = Provenance {
                commit: full.to_owned(),
                toolchain: channel.clone(),
                rustc,
                profile: "release".to_owned(),
                locked: true,
                binary_sha256: sha256_file(&bin).map_err(|why| unmet(&channel, why))?,
                built_at: jiff::Timestamp::now().to_string(),
            };
            let text = serde_json::to_string_pretty(&provenance).unwrap_or_default();
            fs::write(&record, format!("{text}\n"))
                .map_err(|e| unmet(&channel, format!("{}: {e}", record.display())))?;
            Ok(OldBuild {
                short: short.to_owned(),
                bin,
                provenance,
                reused: false,
            })
        }
        Err(why) => {
            let head = git(&["-C", &dir.display().to_string(), "rev-parse", "HEAD"])
                .map_err(|e| unmet(&channel, format!("{why}; {e}")))?;
            let provenance = verified(&record, &bin, full, &head)
                .map_err(|not| unmet(&channel, format!("{why}; {not}")))?;
            Ok(OldBuild {
                short: short.to_owned(),
                bin,
                provenance,
                reused: true,
            })
        }
    }
}

/// The M3 build, built once per test process (and once at a time across them).
pub fn m3() -> &'static OldBuild {
    static BUILD: OnceLock<Result<OldBuild, String>> = OnceLock::new();
    match BUILD.get_or_init(|| old_build(M3_SHORT, M3_COMMIT)) {
        Ok(build) => build,
        Err(why) => panic!("criterion 16 is unmet: {why}"), // ubs:ignore — fails the test
    }
}

/// An old build in a test home, as T1's runner: the home's environment and fake `/proc`
/// exactly as [`Home::std_command`] sets them for this build.
pub struct OldRunner<'a> {
    pub home: &'a Home,
    pub build: &'a OldBuild,
}

impl Runner for OldRunner<'_> {
    fn test_env(&self) -> &TestEnv {
        &self.home.env
    }

    fn command(&self, argv: &[String]) -> Command {
        let mut cmd = Command::new(&self.build.bin);
        self.home.env.apply_to(&mut cmd);
        cmd.env("OMADDONS_HTTP_FIXTURES", &self.home.http);
        if let Some(proc) = &self.home.proc {
            cmd.env("OMADDONS_PROC_ROOT", proc);
        }
        cmd.args(argv);
        cmd
    }
}

// ---------------------------------------------------------------------------------------
// Where things are.

/// `<cache>/omaddons`.
pub fn cache(home: &Home) -> PathBuf {
    home.env.cache().join("omaddons")
}

/// The home's client directories (`_retail_`, `_classic_era_`, …), sorted.
pub fn clients(home: &Home) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = fs::read_dir(&home.root)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.len() > 2 && name.starts_with('_') && name.ends_with('_')
        })
        .map(|e| e.path())
        .collect();
    out.sort();
    out
}

/// A client's `<meta>` (`Interface/.omaddons`).
pub fn meta(client: &Path) -> PathBuf {
    client.join("Interface/.omaddons")
}

/// The entries of `dir`, sorted; none when it is missing.
fn entries(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    out.sort();
    out
}

fn name_of(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------------------
// Unfinished repairs.

/// One `*.repair.jsonl` with no `committed` or `rolled_back` line: its tx and its
/// `prepared` line's `primitives[]`, as JSON (an undecodable journal still names paths).
struct Unfinished {
    tx: String,
    primitives: Vec<Value>,
}

fn unfinished_repairs(home: &Home) -> Vec<Unfinished> {
    let mut out = Vec::new();
    for path in entries(&home.state().join("journal")) {
        let name = name_of(&path);
        let Some(tx) = name.strip_suffix(".repair.jsonl") else {
            continue;
        };
        let text = fs::read_to_string(&path).unwrap_or_default();
        let lines: Vec<Value> = text
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();
        let ended = lines
            .iter()
            .any(|l| matches!(l["type"].as_str(), Some("committed" | "rolled_back")));
        if ended {
            continue;
        }
        let primitives = lines
            .iter()
            .find(|l| l["type"].as_str() == Some("prepared"))
            .and_then(|l| l["primitives"].as_array().cloned())
            .unwrap_or_default();
        out.push(Unfinished {
            tx: tx.to_owned(),
            primitives,
        });
    }
    out
}

fn path_of(value: &Value) -> Option<PathBuf> {
    value.as_str().map(PathBuf::from)
}

fn stamped(endpoint: &Value) -> bool {
    endpoint["stamp"].is_string()
}

/// Every path an unfinished repair journal's `pre` predicate names with a `stamp`
/// (`primitives[].pre.path.stamp`, `.pre.to.stamp`, or a single endpoint's `pre.stamp`):
/// directories not yet renamed, whose mtime is their stamp's, so aging spares them.
pub fn stamped_pre_paths(home: &Home) -> BTreeSet<PathBuf> {
    let mut out = BTreeSet::new();
    for repair in unfinished_repairs(home) {
        for step in &repair.primitives {
            let pre = &step["pre"];
            let pairs = [
                (pre["path"].is_object(), &pre["path"], &step["path"]),
                (pre["to"].is_object(), &pre["to"], &step["to"]),
                (pre["kind"].is_string(), pre, &step["path"]),
            ];
            for (present, endpoint, named) in pairs {
                if present && stamped(endpoint) {
                    out.extend(path_of(named));
                }
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------------------
// Aging.

/// What [`age`] moved, and what it left for the stamps' sake.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Aged {
    pub aged: Vec<PathBuf>,
    pub spared: Vec<PathBuf>,
}

/// The directories whose top-level entries [`age`] moves: those the M3 build reads an
/// age from, and M4's own.
fn aged_dirs(home: &Home) -> Vec<PathBuf> {
    let (state, cache) = (home.state(), cache(home));
    let mut dirs = vec![
        state.join("snapshots"),
        state.join("inflight"),
        state.join("journal"),
        state.join("doctor/runs"),
        cache.join("inflight"),
        cache.join("tmp"),
        cache.join("blobs"),
        cache.join("quarantine"),
        cache.join("plans"),
    ];
    for client in clients(home) {
        dirs.push(meta(&client).join("stage"));
        dirs.push(meta(&client).join("snapshots"));
    }
    dirs
}

/// Sets `path`'s own mtime (a symlink's, never its target's) to `at`.
pub fn set_mtime(path: &Path, at: SystemTime) -> std::io::Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() {
        use rustix::fs::{AtFlags, CWD, Timespec, Timestamps, UTIME_OMIT, utimensat};
        let since = at.duration_since(UNIX_EPOCH).unwrap_or_default();
        let times = Timestamps {
            last_access: Timespec {
                tv_sec: 0,
                tv_nsec: UTIME_OMIT,
            },
            last_modification: Timespec {
                tv_sec: i64::try_from(since.as_secs()).unwrap_or(i64::MAX),
                tv_nsec: i64::from(since.subsec_nanos()),
            },
        };
        return utimensat(CWD, path, &times, AtFlags::SYMLINK_NOFOLLOW).map_err(Into::into);
    }
    fs::File::open(path)?.set_modified(at)
}

/// Two hours pass for the M3 build: every top-level entry of the directories it judges by
/// age (and of M4's own, and `<state>/doctor/latest`) gets an mtime [`AGE`] back, except
/// the paths in `spare`. Never below a top-level entry, never `Interface/AddOns/` or
/// `WTF/`; missing directories are skipped.
pub fn age(home: &Home, spare: &BTreeSet<PathBuf>) -> Aged {
    let past = SystemTime::now() - AGE;
    let mut targets: Vec<PathBuf> = aged_dirs(home).iter().flat_map(|d| entries(d)).collect();
    let latest = home.state().join("doctor/latest");
    if fs::symlink_metadata(&latest).is_ok() {
        targets.push(latest);
    }
    let mut out = Aged::default();
    for path in targets {
        if spare.contains(&path) {
            out.spared.push(path);
        } else {
            set_mtime(&path, past).unwrap();
            out.aged.push(path);
        }
    }
    out
}

// ---------------------------------------------------------------------------------------
// The protected inventory.

/// One protected entry by what it holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Hashed {
    File { sha256: String, mode: u32 },
    Dir { tree: String },
    Link { target: PathBuf },
}

impl Hashed {
    /// What `path` holds now; `None` when it is missing.
    pub fn of(path: &Path) -> Option<Self> {
        let meta = fs::symlink_metadata(path).ok()?;
        Some(if meta.file_type().is_symlink() {
            Self::Link {
                target: fs::read_link(path).ok()?,
            }
        } else if meta.is_dir() {
            Self::Dir {
                tree: tree_hash(path),
            }
        } else {
            Self::File {
                sha256: sha256_file(path).ok()?,
                mode: meta.permissions().mode() & 0o7777,
            }
        })
    }
}

/// What M4 wrote that M4 reads again, by path: the entries the M3 build must never
/// delete or overwrite.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct M4Inventory(pub BTreeMap<PathBuf, Hashed>);

/// What changed among an inventory's entries.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct InventoryDiff {
    pub deleted: Vec<PathBuf>,
    pub overwritten: Vec<PathBuf>,
}

impl InventoryDiff {
    pub fn is_empty(&self) -> bool {
        self.deleted.is_empty() && self.overwritten.is_empty()
    }

    /// `unchanged`, or `<n> deleted, <m> overwritten: <paths>` (relative, cut at `WTF`).
    pub fn describe(&self, home: &Home) -> String {
        if self.is_empty() {
            return "unchanged".to_owned();
        }
        let paths: Vec<String> = self
            .deleted
            .iter()
            .map(|p| format!("-{}", rel(home, p)))
            .chain(
                self.overwritten
                    .iter()
                    .map(|p| format!("~{}", rel(home, p))),
            )
            .collect();
        format!(
            "{} deleted, {} overwritten: {}",
            self.deleted.len(),
            self.overwritten.len(),
            paths.join(", ")
        )
    }
}

impl M4Inventory {
    /// Every protected entry of `home`:
    /// - each `<state>/journal/*.repair.jsonl` and `*.reactivating.jsonl`;
    /// - each entry of `<state>/doctor/runs/` (a run or a staged run, hashed whole) and
    ///   every other entry of `<state>/doctor/` (`latest`, temp names);
    /// - each client's `<meta>/snapshots/*.repair`;
    /// - each top-level entry of `<cache>/quarantine/` and `<cache>/plans/`;
    /// - each existing path an unfinished repair's primitives name (`path`, `to`), except
    ///   that repair's own `<state>/inflight/<tx>.lock`.
    ///
    /// A committed repair's subjects are added with [`M4Inventory::subject`].
    pub fn collect(home: &Home) -> Self {
        let state = home.state();
        let mut paths: Vec<PathBuf> = entries(&state.join("journal"))
            .into_iter()
            .filter(|p| {
                let name = name_of(p);
                name.ends_with(".repair.jsonl") || name.ends_with(".reactivating.jsonl")
            })
            .collect();
        let doctor = state.join("doctor");
        let runs = doctor.join("runs");
        paths.extend(entries(&doctor).into_iter().filter(|p| *p != runs));
        paths.extend(entries(&runs));
        for client in clients(home) {
            paths.extend(
                entries(&meta(&client).join("snapshots"))
                    .into_iter()
                    .filter(|p| name_of(p).ends_with(".repair")),
            );
        }
        paths.extend(entries(&cache(home).join("quarantine")));
        paths.extend(entries(&cache(home).join("plans")));
        for repair in unfinished_repairs(home) {
            let own = state.join("inflight").join(format!("{}.lock", repair.tx));
            for step in &repair.primitives {
                for key in ["path", "to"] {
                    paths.extend(path_of(&step[key]).filter(|p| *p != own));
                }
            }
        }
        let mut inventory = Self::default();
        for path in paths {
            inventory.subject(&path);
        }
        inventory
    }

    /// Protects `path` as it is now (nothing when it is missing).
    pub fn subject(&mut self, path: &Path) {
        if let Some(hashed) = Hashed::of(path) {
            self.0.insert(path.to_path_buf(), hashed);
        }
    }

    /// The same entries as they are now; a missing one is left out.
    pub fn rehash(&self) -> Self {
        Self(
            self.0
                .keys()
                .filter_map(|p| Hashed::of(p).map(|h| (p.clone(), h)))
                .collect(),
        )
    }

    /// The entries of `self` that `after` lacks (deleted) or holds otherwise (overwritten).
    pub fn compare(&self, after: &Self) -> InventoryDiff {
        let mut diff = InventoryDiff::default();
        for (path, hashed) in &self.0 {
            match after.0.get(path) {
                None => diff.deleted.push(path.clone()),
                Some(now) if now != hashed => diff.overwritten.push(path.clone()),
                Some(_) => {}
            }
        }
        diff
    }

    /// The inventory without the entries `exempt` names.
    pub fn without(&self, exempt: impl Fn(&Path) -> bool) -> Self {
        Self(
            self.0
                .iter()
                .filter(|(p, _)| !exempt(p))
                .map(|(p, h)| (p.clone(), h.clone()))
                .collect(),
        )
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Its paths, relative and cut at `WTF`, for a log.
    pub fn rel(&self, home: &Home) -> Vec<String> {
        self.0.keys().map(|p| rel(home, p)).collect()
    }
}

// ---------------------------------------------------------------------------------------
// The bytes check.

/// Addon folders and `WTF` trees by content (`tree_hash`) with where each was found: a
/// legitimate step may move a folder (a rollback, a quarantine, an older build's own
/// snapshot), and only content shows a loss through such a step.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct FolderBytes(pub BTreeMap<String, Vec<PathBuf>>);

/// An addon folder (directly holding a file named `*.toc`, any case) or a `WTF` tree (a
/// directory named `WTF` or `WTF.previous`).
fn is_tree(dir: &Path) -> bool {
    let name = name_of(dir);
    if name == "WTF" || name == "WTF.previous" {
        return true;
    }
    entries(dir).iter().any(|p| {
        fs::symlink_metadata(p).is_ok_and(|m| m.is_file())
            && name_of(p).to_ascii_lowercase().ends_with(".toc")
    })
}

/// Every tree at or below `root` (symlinks never followed) into `found`.
fn trees_into(root: &Path, found: &mut BTreeMap<String, Vec<PathBuf>>) {
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        if !fs::symlink_metadata(&dir).is_ok_and(|m| m.is_dir()) {
            continue;
        }
        if is_tree(&dir) {
            found.entry(tree_hash(&dir)).or_default().push(dir.clone());
        }
        pending.extend(entries(&dir));
    }
}

impl FolderBytes {
    /// The trees inside the inventory's entries (or that are one).
    pub fn held(_home: &Home, inventory: &M4Inventory) -> Self {
        let mut found = BTreeMap::new();
        for (path, hashed) in &inventory.0 {
            if matches!(hashed, Hashed::Dir { .. }) {
                trees_into(path, &mut found);
            }
        }
        Self::sorted(found)
    }

    /// Every tree under the home's TestEnv root: the client dirs, `<state>`, `<cache>`,
    /// `<config>` and the `src/` folders scenes install from.
    pub fn anywhere(home: &Home) -> Self {
        let mut found = BTreeMap::new();
        trees_into(home.env.root(), &mut found);
        Self::sorted(found)
    }

    fn sorted(mut found: BTreeMap<String, Vec<PathBuf>>) -> Self {
        for paths in found.values_mut() {
            paths.sort();
        }
        Self(found)
    }

    /// Each digest of `self` that `now` holds nowhere, with its former paths.
    pub fn lost(&self, now: &Self) -> Vec<(String, Vec<PathBuf>)> {
        self.0
            .iter()
            .filter(|(digest, _)| !now.0.contains_key(*digest))
            .map(|(d, p)| (d.clone(), p.clone()))
            .collect()
    }

    /// Without the digests found only at paths `exempt` names.
    pub fn without(&self, exempt: impl Fn(&Path) -> bool) -> Self {
        Self(
            self.0
                .iter()
                .filter(|(_, paths)| !paths.iter().all(|p| exempt(p)))
                .map(|(d, p)| (d.clone(), p.clone()))
                .collect(),
        )
    }
}

/// `accounted`, or `<n> lost: <digest> at <paths>; …` (relative, cut at `WTF`).
pub fn describe_lost(home: &Home, lost: &[(String, Vec<PathBuf>)]) -> String {
    if lost.is_empty() {
        return "accounted".to_owned();
    }
    let each: Vec<String> = lost
        .iter()
        .map(|(digest, paths)| {
            let at: Vec<String> = paths.iter().map(|p| rel(home, p)).collect();
            format!("{} at {}", &digest[..12.min(digest.len())], at.join(", "))
        })
        .collect();
    format!("{} lost: {}", lost.len(), each.join("; "))
}

/// History is only ever appended to: `before` is a byte prefix of `after`.
pub fn history_prefix_kept(before: &[u8], after: &[u8]) -> bool {
    after.starts_with(before)
}

// ---------------------------------------------------------------------------------------
// The table.

/// One replica's record, as README's table quotes it.
#[derive(Debug, Clone, Serialize)]
pub struct Row {
    /// `committed`, `interrupted`, `undo`, `apply`, `set-aside`, `reactivation` or
    /// `control`.
    pub family: &'static str,
    pub scene: String,
    /// `at rest`, `prepared`, `before 2`, `after 2`, `done`, `history` or `committed`.
    pub position: String,
    /// `at once` or `2 h`.
    pub age: &'static str,
    /// The M3 command, as a person would type it.
    pub m3: String,
    pub exit: i32,
    /// `error.code`, or `ok`.
    pub code: String,
    /// The M3 run's warning codes, in order.
    pub warnings: Vec<String>,
    /// `unchanged`, or `<n> deleted, <m> overwritten: <paths>`.
    pub protected: String,
    /// `accounted`, or `<n> lost: <digests and former paths>`.
    pub bytes: String,
    /// Everything else that changed.
    pub other: Vec<String>,
    /// What the M4 build's next commands did.
    pub m4_next: String,
    /// `untouched`, `recovered_by_m3` or `fail`.
    pub class: &'static str,
}

/// The table's columns: [`Row`]'s fields, in order.
pub const COLUMNS: [&str; 13] = [
    "family",
    "scene",
    "position",
    "age",
    "m3",
    "exit",
    "code",
    "warnings",
    "protected",
    "bytes",
    "other",
    "m4_next",
    "class",
];

/// Every path below a `WTF` or `WTF.previous` component in `text` cut to `WTF/…`, so no
/// account, realm or character name reaches a table.
pub fn cut_wtf(text: &str) -> String {
    const ENDS: [char; 10] = [' ', ',', '|', '"', '`', ')', ']', ';', '\n', '\t'];
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("WTF") {
        let starts = at == 0 || rest[..at].ends_with('/') || rest[..at].ends_with(ENDS);
        let after = &rest[at + 3..];
        let marker = if after.starts_with('/') {
            Some(4)
        } else if after.starts_with(".previous/") {
            Some(13)
        } else {
            None
        };
        match marker.filter(|_| starts) {
            Some(len) => {
                out.push_str(&rest[..at + len]);
                out.push('…');
                let tail = &rest[at + len..];
                let end = tail.find(ENDS).unwrap_or(tail.len());
                rest = &tail[end..];
            }
            None => {
                out.push_str(&rest[..at + 3]);
                rest = &rest[at + 3..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn cell(text: &str) -> String {
    cut_wtf(text).replace('|', "\\|").replace('\n', " ")
}

/// The rows as a GitHub Markdown table under `title`: one row per replica, `|` escaped,
/// every path cut at `WTF`.
pub fn table(title: &str, rows: &[Row]) -> String {
    let mut out = format!("### {}\n\n| {} |\n|", cell(title), COLUMNS.join(" | "));
    out.push_str(&"---|".repeat(COLUMNS.len()));
    out.push('\n');
    for r in rows {
        let cells = [
            r.family.to_owned(),
            r.scene.clone(),
            r.position.clone(),
            r.age.to_owned(),
            r.m3.clone(),
            r.exit.to_string(),
            r.code.clone(),
            r.warnings.join(", "),
            r.protected.clone(),
            r.bytes.clone(),
            r.other.join("; "),
            r.m4_next.clone(),
            r.class.to_owned(),
        ];
        let cells: Vec<String> = cells.iter().map(|c| cell(c)).collect();
        out.push_str(&format!("| {} |\n", cells.join(" | ")));
    }
    out
}

/// [`table`] on stderr, written directly as `tests/compat_m2.rs` prints its table.
pub fn print_table(title: &str, rows: &[Row]) {
    let _ = writeln!(std::io::stderr(), "\n{}", table(title, rows));
}

/// `path` relative to the home's TestEnv root, every component after a `WTF` (or
/// `WTF.previous`) one replaced by `…`.
pub fn rel(home: &Home, path: &Path) -> String {
    let path = path.strip_prefix(home.env.root()).unwrap_or(path);
    let mut out: Vec<String> = Vec::new();
    for component in path.components() {
        let text = match component {
            Component::Normal(name) => name.to_string_lossy().into_owned(),
            Component::RootDir => String::new(),
            other => other.as_os_str().to_string_lossy().into_owned(),
        };
        let wtf = text == "WTF" || text == "WTF.previous";
        out.push(text);
        if wtf {
            out.push("…".to_owned());
            break;
        }
    }
    out.join("/")
}
