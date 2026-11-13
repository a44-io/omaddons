//! `README.md` stays whole on GitHub. GitHub stops rendering a README at about 500 KiB
//! (512,000 bytes), and a longer one shows cut off on the repository page with no other
//! sign. The README is the product contract and the agents' onboarding document, kept
//! as one file by choice, and every change that adds behavior adds to it (omad-svxx
//! condensed it to 478,477 bytes; omad-06poz added this bound).

use std::path::Path;

/// The largest `README.md` the gate accepts: about 12 KB below GitHub's cutoff, which is
/// approximate. Change it only with a comment saying where a new number came from.
const README_MAX_BYTES: u64 = 500_000;
const _: () = assert!(README_MAX_BYTES < 500 * 1024, "below GitHub's cutoff");

/// Whether a README of `bytes` bytes fits, and if not, what to do about it.
fn readme_fits(bytes: u64) -> Result<(), String> {
    if bytes <= README_MAX_BYTES {
        return Ok(());
    }
    Err(format!(
        "README.md is {bytes} bytes, over the {README_MAX_BYTES} the gate accepts: GitHub stops \
         rendering a README at about 500 KiB, so the page would be cut off. Condense it by \
         hand, as omad-svxx did: repetition, near-identical example runs and filler go; every \
         command, flag, code, key, path, limit and guarantee stays. Never split the document."
    ))
}

#[test]
fn the_bound_holds_at_its_edges() {
    // (case, size in bytes, whether it fits)
    let cases = [
        ("one byte under the bound", README_MAX_BYTES - 1, true),
        ("exactly the bound", README_MAX_BYTES, true),
        ("one byte over the bound", README_MAX_BYTES + 1, false),
    ];
    for (case, bytes, fits) in cases {
        let verdict = readme_fits(bytes);
        assert_eq!(
            verdict.is_ok(),
            fits,
            "{case}: {bytes} bytes gave {verdict:?}"
        );
    }
    let refusal = readme_fits(README_MAX_BYTES + 1).unwrap_err();
    for needed in ["500001 bytes", "500000", "500 KiB", "Condense it by hand"] {
        assert!(
            refusal.contains(needed),
            "{needed:?} missing from: {refusal}"
        );
    }
}

#[test]
fn readme_fits_on_github() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("README.md");
    let bytes = std::fs::metadata(&path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        .len();
    if let Err(message) = readme_fits(bytes) {
        panic!("{message}");
    }
}
