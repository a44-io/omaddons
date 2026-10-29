//! Three outputs, computed separately:
//!
//! - The digest of the widget's runtime files under `omarchy/` (its tests, a generated
//!   `Build.js` and the manifest aside: the same set as `src/omarchy/assets.rs`), so
//!   `status` can compare an installed widget with this build on every call without
//!   hashing some 230 KB each time (E4). `src/omarchy/render.rs` holds it equal to a
//!   digest of the embedded assets in a unit test.
//! - `OMADDONS_BUILD_ID`, the build's identity (M4-D19): a digest of the explicit input
//!   manifest in `src/build_info/inputs.rs` and the toolchain. It never covers `skills/`
//!   (M4-D7).
//! - `OMADDONS_GUIDE_SHA256`, the committed skill's digest with its stamp line omitted
//!   (M4-D7), so the binary knows which guide it prints without rendering it.

// Cargo reads its directives from the build script's stdout.
#![allow(clippy::print_stdout)]

use std::error::Error;
use std::fs;
use std::path::Path;

use sha2::{Digest, Sha256};

#[path = "src/build_info/inputs.rs"]
#[allow(
    dead_code,
    reason = "the library's tests use the classification; the build only hashes"
)]
mod inputs;

// One stamp algorithm for the renderer, install-skill, the doctor and this script: the
// very same file.
#[path = "src/contract/guide/stamp.rs"]
#[allow(
    dead_code,
    reason = "the library verifies stamps; the build only computes one digest"
)]
mod stamp;

/// The committed skill, which `tests/skill.rs` writes from the rendered guide.
const SKILL: &str = "skills/omaddons/SKILL.md";

fn main() -> Result<(), Box<dyn Error>> {
    widget_digest()?;
    build_id()?;
    guide_digest();
    Ok(())
}

/// `OMADDONS_GUIDE_SHA256`: the digest of the committed skill's content without its stamp
/// line, never its recorded digits. A missing or unstamped file does not fail the build,
/// because the binary must build to write it: the value is then 64 zeros, with a warning,
/// and `tests/skill.rs` fails until the file is regenerated.
fn guide_digest() {
    println!("cargo::rerun-if-changed={SKILL}");
    println!("cargo::rerun-if-changed=src/contract/guide/stamp.rs");
    let digest = fs::read_to_string(SKILL)
        .ok()
        .map(|text| stamp::digest_without_stamp(&text));
    let hex = match digest {
        Some(Ok(digest)) => stamp::hex(&digest),
        _ => {
            println!(
                "cargo::warning={SKILL} is missing or unstamped; run OMADDONS_UPDATE_SKILL=1 cargo test --test skill"
            );
            "0".repeat(64)
        }
    };
    println!("cargo::rustc-env=OMADDONS_GUIDE_SHA256={hex}");
}

/// `OMADDONS_BUILD_ID` from the input manifest and the toolchain Cargo describes.
fn build_id() -> Result<(), Box<dyn Error>> {
    let root = Path::new(".");
    // `omarchy` is watched by the widget digest already. `.cargo/config.toml` is watched
    // only while it exists: creating it later is picked up the next time any other
    // input changes.
    for path in inputs::rerun_paths(root) {
        if path != "omarchy" {
            println!("cargo::rerun-if-changed={path}");
        }
    }
    let var = |key: &str| std::env::var(key).unwrap_or_default();
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_owned());
    let version = std::process::Command::new(rustc).arg("-V").output()?;
    let mut features: Vec<String> = std::env::vars()
        .filter_map(|(key, _)| key.strip_prefix("CARGO_FEATURE_").map(str::to_owned))
        .collect();
    features.sort();
    let debug_assertions = std::env::var_os("CARGO_CFG_DEBUG_ASSERTIONS").is_some();
    let toolchain = inputs::Toolchain {
        rustc_version: String::from_utf8(version.stdout)?.trim().to_owned(),
        target: var("TARGET"),
        // `PROFILE` reads `release` for `bench-fixtures`, whose binary reads failpoints.
        profile: vec![
            ("PROFILE".to_owned(), var("PROFILE")),
            ("OPT_LEVEL".to_owned(), var("OPT_LEVEL")),
            ("DEBUG".to_owned(), var("DEBUG")),
            (
                "debug_assertions".to_owned(),
                if debug_assertions { "1" } else { "0" }.to_owned(),
            ),
        ],
        features,
        rustflags: var("CARGO_ENCODED_RUSTFLAGS"),
    };
    let id = inputs::input_digest(root, &toolchain)?;
    println!("cargo::rustc-env=OMADDONS_BUILD_ID={id}");
    Ok(())
}

/// `OUT_DIR/assets_digest.rs`: the widget's runtime files' digest.
fn widget_digest() -> Result<(), Box<dyn Error>> {
    println!("cargo::rerun-if-changed=omarchy");
    let root = Path::new("omarchy");
    let mut files = Vec::new();
    collect(root, root, &mut files)?;
    files.sort();
    let mut digest = Sha256::new();
    field(&mut digest, b"omaddons widget assets 1");
    for (path, bytes) in &files {
        field(&mut digest, path.as_bytes());
        field(&mut digest, bytes);
    }
    let out = Path::new(&std::env::var("OUT_DIR")?).join("assets_digest.rs");
    fs::write(
        out,
        format!(
            "/// The digest of the widget's runtime files, the manifest aside (`build.rs`).\n\
             pub const ASSETS_DIGEST: [u8; 32] = {:?};\n",
            digest.finalize().as_slice()
        ),
    )?;
    Ok(())
}

/// Every file under `dir` by its path from `root`, as `assets.rs` embeds them.
fn collect(
    root: &Path,
    dir: &Path,
    out: &mut Vec<(String, Vec<u8>)>,
) -> Result<(), Box<dyn Error>> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        let relative = path
            .strip_prefix(root)?
            .to_str()
            .ok_or("a widget file name that is not UTF-8")?
            .to_owned();
        if matches!(relative.as_str(), "tests" | "Build.js" | "manifest.json") {
            continue;
        }
        if path.is_dir() {
            collect(root, &path, out)?;
        } else {
            out.push((relative, fs::read(&path)?));
        }
    }
    Ok(())
}

/// One length-prefixed field, as `render.rs`'s `Fields` frames them.
fn field(digest: &mut Sha256, bytes: &[u8]) {
    digest.update((bytes.len() as u64).to_be_bytes());
    digest.update(bytes);
}
