//! The running binary's own path, and the path the bar widget runs it by
//! (omad-widget-plugin-3bcbh.2). A fix run's `undo.sh` names the binary that wrote it,
//! `omarchy install` stamps the widget with it, and `status` tells the widget which path
//! to run, so a widget that finds omaddons on `PATH` (through a mise shim, say) can run
//! this binary directly. Reads only: `current_exe`, then `canonicalize`, never a
//! subprocess or the environment.

use std::path::{Path, PathBuf};

use crate::error::{Error, Result, Suggestion};

/// The running binary, canonical: what the widget's commands run, and what a fix run's
/// `undo.sh` runs.
pub(crate) fn current_binary() -> Result<PathBuf> {
    let exe = std::env::current_exe().map_err(|err| {
        Error::io(
            Path::new("/proc/self/exe"),
            &err,
            Suggestion::text("run omaddons from its installed path"),
        )
    })?;
    std::fs::canonicalize(&exe).map_err(|err| {
        Error::io(
            &exe,
            &err,
            Suggestion::text("run omaddons from its installed path"),
        )
    })
}

/// The binary the widget runs, `Build.js`'s `BINARY` (omad-vd1z) and `status`'s
/// `widget.binary`: [`current_binary`], unless mise installed it. mise keeps each version
/// in `<installs>/<tool>/<version>/` and links `<installs>/<tool>/latest` to the newest,
/// so the path is then this same file by its `latest` path. A version mise installs later
/// is the one the widget runs, and its version change reinstalls the widget (M3-D42).
/// Never the `omarchy-mise-install` wrapper on `PATH`, which asks mise for newer releases
/// on every run and, offline, waits on the network for over a minute.
pub(crate) fn widget_binary() -> Result<PathBuf> {
    let binary = current_binary()?;
    Ok(mise_latest(&binary).unwrap_or(binary))
}

/// `binary` (canonical) as `<…>/installs/<tool>/latest/<rest>`, when it is
/// `<…>/installs/<tool>/<version>/<rest>` and that `latest` path leads to this very file.
/// Structural: no environment is read, so a custom `MISE_DATA_DIR` works the same.
fn mise_latest(binary: &Path) -> Option<PathBuf> {
    for version in binary.ancestors().skip(1) {
        let tool = version.parent()?;
        let installs = tool.parent()?;
        if installs.file_name()? != "installs" {
            continue;
        }
        if version.file_name()? == "latest" {
            return None;
        }
        let rest = binary.strip_prefix(version).ok()?;
        let latest = tool.join("latest").join(rest);
        let same = std::fs::canonicalize(&latest).ok()? == binary;
        return same.then_some(latest);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A binary file at `rel` under `root`, by its canonical path.
    fn binary_at(root: &Path, rel: &str) -> PathBuf {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"\x7fELF").unwrap();
        std::fs::canonicalize(path).unwrap()
    }

    /// A mise-installed binary is stamped by mise's `latest` link to its tool (omad-vd1z),
    /// whatever folder the release unpacked it in, so the widget runs the newest version
    /// mise installs; anything else keeps its canonical path.
    #[test]
    fn a_mise_installed_binary_is_stamped_by_its_latest_link() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let tool = root.join("mise/installs/github-a44-io-omaddons");

        // Flat, and nested in the release's own folder.
        let flat = binary_at(&tool, "0.2.0/omaddons");
        let nested = binary_at(&tool, "0.2.0/omaddons-x86_64/omaddons");
        symlink("./0.2.0", tool.join("latest")).unwrap();
        assert_eq!(mise_latest(&flat), Some(tool.join("latest/omaddons")));
        assert_eq!(
            mise_latest(&nested),
            Some(tool.join("latest/omaddons-x86_64/omaddons"))
        );

        // An older version beside the newest that `latest` names is not it: a pin, or a
        // version mise has not pruned, runs as itself.
        let older = binary_at(&tool, "0.1.0/omaddons");
        assert_eq!(mise_latest(&older), None);

        // Under `latest` itself (never canonical, but still), outside any installs folder,
        // a tool without a `latest` link, or a version folder with nothing below it.
        assert_eq!(mise_latest(&tool.join("latest/omaddons")), None);
        assert_eq!(mise_latest(&binary_at(&root, "usr/bin/omaddons")), None);
        let unlinked = root.join("mise/installs/other");
        assert_eq!(mise_latest(&binary_at(&unlinked, "1.0.0/omaddons")), None);
        assert_eq!(mise_latest(&tool.join("0.2.0")), None);
        assert_eq!(mise_latest(Path::new("/")), None);
    }
}
