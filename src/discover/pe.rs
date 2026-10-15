//! A Windows executable's version from its `VS_FIXEDFILEINFO` numeric fields (the
//! version string form is unverified and never read), via `pelite` over a read-only
//! memory map. A real 3.3.5a `Wow.exe` reports file version 3.3.5.12340 and product
//! version 3.3.0.0, so the file version wins and the product version is only a
//! fallback when the file version is all zeros.

use std::path::Path;

use crate::rules::ClientVersion;

/// The exe's version; `None` when it is not a readable PE with a version resource or
/// both versions are zero.
pub fn exe_version(path: &Path) -> Option<ClientVersion> {
    // An exe is mapped, never read whole; `open_regular` keeps FIFOs and devices out.
    drop(super::open_regular(path)?);
    let map = pelite::FileMap::open(path).ok()?;
    let pe = pelite::PeFile::from_bytes(&map).ok()?;
    let info = pe.resources().ok()?.version_info().ok()?;
    let fixed = info.fixed()?;
    [fixed.dwFileVersion, fixed.dwProductVersion]
        .into_iter()
        .find(|v| [v.Major, v.Minor, v.Patch, v.Build] != [0; 4])
        .map(|v| ClientVersion {
            major: u32::from(v.Major),
            minor: u32::from(v.Minor),
            patch: u32::from(v.Patch),
            build: Some(u32::from(v.Build)),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_pe_files_have_no_version() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("Wow.exe");
        assert_eq!(exe_version(&exe), None, "missing");
        std::fs::write(&exe, b"").unwrap();
        assert_eq!(exe_version(&exe), None, "empty");
        std::fs::write(&exe, b"MZ not really a PE file at all").unwrap();
        assert_eq!(exe_version(&exe), None, "not a PE");
        assert_eq!(exe_version(dir.path()), None, "a directory");
    }
}
