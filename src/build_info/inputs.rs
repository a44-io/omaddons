//! The build id's explicit input manifest (M4-D19): everything that affects the binary,
//! hashed by `build.rs` into `OMADDONS_BUILD_ID`. The library shares this file so that a
//! test reads the very list the build hashes, and checks that every top-level name of
//! the repository is either an input or excluded with its reason.
//!
//! `build.rs` compiles this file too, so it uses only `std` and `sha2`. It reads files,
//! and never spawns a process or writes one (the compiler's version comes from
//! `build.rs`).

use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use sha2::{Digest, Sha256};

/// One input of the build id, by its path from the crate root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input {
    /// A file the build reads: missing, the build fails.
    File(&'static str),
    /// A file the build reads when it exists (Cargo's project config).
    OptionalFile(&'static str),
    /// Every entry under a directory.
    Tree(&'static str),
}

impl Input {
    pub const fn path(self) -> &'static str {
        match self {
            Self::File(path) | Self::OptionalFile(path) | Self::Tree(path) => path,
        }
    }

    /// The top-level name it is under (`.cargo` for `.cargo/config.toml`).
    pub fn top_level(self) -> &'static str {
        let path = self.path();
        path.split_once('/').map_or(path, |(first, _)| first)
    }
}

/// What the build id covers, in hashing order.
pub const BUILD_ID_INPUTS: &[Input] = &[
    Input::File("Cargo.toml"),
    Input::File("Cargo.lock"),
    Input::File("build.rs"),
    Input::File("rust-toolchain.toml"),
    Input::OptionalFile(".cargo/config.toml"),
    Input::Tree("src"),
    // The schemars facade and its no-op derive (omad-ho1p), path dependencies of the build.
    Input::Tree("crates"),
    Input::Tree("catalog"),
    Input::Tree("omarchy"),
];

/// Top-level names that never feed the build id, each with why; `*.<ext>` matches a
/// suffix. `skills` above all: the guide is generated from the build, so hashing it
/// would leave no fixed point (M4-D7).
pub const BUILD_ID_EXCLUDED: &[(&str, &str)] = &[
    (".git", "the repository"),
    ("target", "build output"),
    (".beads", "the issue tracker"),
    (
        "tests",
        "integration tests and fixtures; unit tests' include_*! of fixtures are test-only",
    ),
    (
        "skills",
        "generated guide output, which embeds the build id (M4-D7)",
    ),
    ("docs", "prose"),
    ("fuzz", "its own crate"),
    ("scripts", "gate and widget scripts"),
    ("proptest-regressions", "test seeds"),
    ("README.md", "prose"),
    ("LICENSE", "the license text; nothing compiles it in"),
    ("AGENTS.md", "prose"),
    ("CLAUDE.md", "prose"),
    ("Argcfile.sh", "the task runner"),
    (
        "clippy.toml",
        "lint policy: whether the gate passes, never the binary's bytes",
    ),
    (
        "rustfmt.toml",
        "format policy: whether the gate passes, never the binary's bytes",
    ),
    (
        "deny.toml",
        "licence and advisory policy: whether the gate passes, never the binary's bytes",
    ),
    (
        ".rumdl.toml",
        "Markdown lint and format policy: how prose is committed, never the binary's bytes",
    ),
    (".gitignore", "the repository's ignore list"),
    (".claude", "agent settings"),
    (".rumdl_cache", "a Markdown linter's cache"),
    ("lcov.info", "coverage output"),
    ("*.profraw", "coverage output"),
    ("*.profdata", "coverage output"),
];

/// The compiler, target, profile, features and flags the build ran with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toolchain {
    /// `rustc -V`.
    pub rustc_version: String,
    pub target: String,
    /// `KEY=VALUE` pairs in a fixed order: `PROFILE`, `OPT_LEVEL`, `DEBUG`,
    /// `debug_assertions`.
    pub profile: Vec<(String, String)>,
    /// The enabled features' names (any order).
    pub features: Vec<String>,
    /// `CARGO_ENCODED_RUSTFLAGS`, empty when unset.
    pub rustflags: String,
}

/// The build id of the tree at `root` built with `toolchain`: the first 16 lowercase hex
/// digits of a SHA-256 over length-prefixed fields: every file input in list order, then
/// every entry of every tree input in bytewise path order (a file's bytes, a symlink's
/// target never followed, another kind's name; directories are walked, not hashed),
/// then the toolchain. Modification times never count. A missing file or tree is an
/// error; a missing optional file is skipped.
pub fn input_digest(root: &Path, toolchain: &Toolchain) -> io::Result<String> {
    let mut digest = Sha256::new();
    field(&mut digest, b"omaddons build id 1");
    for input in BUILD_ID_INPUTS {
        match *input {
            Input::File(path) => {
                field(&mut digest, path.as_bytes());
                field(&mut digest, &fs::read(root.join(path))?);
            }
            Input::OptionalFile(path) => match fs::read(root.join(path)) {
                Ok(bytes) => {
                    field(&mut digest, path.as_bytes());
                    field(&mut digest, &bytes);
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            },
            Input::Tree(path) => {
                let mut entries = Vec::new();
                walk(root, path.as_bytes().to_vec(), &mut entries)?;
                entries.sort();
                for (rel, kind, payload) in entries {
                    field(&mut digest, kind.as_bytes());
                    field(&mut digest, &rel);
                    field(&mut digest, &payload);
                }
            }
        }
    }
    field(&mut digest, toolchain.rustc_version.as_bytes());
    field(&mut digest, toolchain.target.as_bytes());
    for (key, value) in &toolchain.profile {
        field(&mut digest, format!("{key}={value}").as_bytes());
    }
    let mut features: Vec<&String> = toolchain.features.iter().collect();
    features.sort();
    field(&mut digest, features.len().to_string().as_bytes());
    for feature in features {
        field(&mut digest, feature.as_bytes());
    }
    field(&mut digest, toolchain.rustflags.as_bytes());
    Ok(digest
        .finalize()
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// One tree entry: its path from the crate root (`/`-separated bytes), its kind and
/// what is hashed for it.
type TreeEntry = (Vec<u8>, &'static str, Vec<u8>);

/// Every entry under `root/rel`, directories walked.
fn walk(root: &Path, rel: Vec<u8>, out: &mut Vec<TreeEntry>) -> io::Result<()> {
    let dir = root.join(std::ffi::OsStr::from_bytes(&rel));
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let mut path = rel.clone();
        path.push(b'/');
        path.extend_from_slice(entry.file_name().as_bytes());
        let kind = entry.file_type()?;
        if kind.is_dir() {
            walk(root, path, out)?;
        } else if kind.is_symlink() {
            let target = fs::read_link(entry.path())?;
            out.push((path, "symlink", target.as_os_str().as_bytes().to_vec()));
        } else if kind.is_file() {
            let bytes = fs::read(entry.path())?;
            out.push((path, "file", bytes));
        } else {
            out.push((path, "other", Vec::new()));
        }
    }
    Ok(())
}

/// What `build.rs` watches: every file and tree input, and an optional file only when it
/// exists (Cargo reruns a build script on every build for a watched path that does not
/// exist).
pub fn rerun_paths(root: &Path) -> Vec<&'static str> {
    BUILD_ID_INPUTS
        .iter()
        .filter(|input| match input {
            Input::OptionalFile(path) => fs::symlink_metadata(root.join(path)).is_ok(),
            Input::File(_) | Input::Tree(_) => true,
        })
        .map(|input| input.path())
        .collect()
}

/// The names of `names` (top-level names of the repository) that are neither an input's
/// top-level name nor excluded.
pub fn unclassified<'a>(names: impl IntoIterator<Item = &'a str>) -> Vec<&'a str> {
    names
        .into_iter()
        .filter(|name| {
            let input = BUILD_ID_INPUTS.iter().any(|i| i.top_level() == *name);
            let excluded = BUILD_ID_EXCLUDED.iter().any(|(pattern, _)| {
                pattern
                    .strip_prefix('*')
                    .map_or(pattern == name, |suffix| name.ends_with(suffix))
            });
            !input && !excluded
        })
        .collect()
}

/// One length-prefixed field: a big-endian `u64` length, then the bytes.
fn field(digest: &mut Sha256, bytes: &[u8]) {
    digest.update((bytes.len() as u64).to_be_bytes());
    digest.update(bytes);
}
