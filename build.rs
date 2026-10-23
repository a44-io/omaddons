//! The digest of the widget's runtime files under `omarchy/` (its tests, a generated
//! `Build.js` and the manifest aside: the same set as `src/omarchy/assets.rs`), so
//! `status` can compare an installed widget with this build on every call without
//! hashing some 230 KB each time (E4). `src/omarchy/render.rs` holds it equal to a
//! digest of the embedded assets in a unit test.

// Cargo reads its directives from the build script's stdout.
#![allow(clippy::print_stdout)]

use std::error::Error;
use std::fs;
use std::path::Path;

use sha2::{Digest, Sha256};

fn main() -> Result<(), Box<dyn Error>> {
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
