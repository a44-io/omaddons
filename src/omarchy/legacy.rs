//! A widget an older omaddons rendered (M3-D7, retired by omad-widget-plugin-3bcbh.9):
//! its manifest's entry points name a code folder `r-<hex>/` holding a generated
//! `Build.js`, one `var` per line, which recorded the binary its commands ran, the tool
//! version, the plugin id, the data context it served and a content hash. omaddons reads
//! one back only to recognise it, which `omarchy install` moves out for the git plugin,
//! and to say whose state it served.

use std::io::Read;
use std::path::{Component, Path, PathBuf};

use serde_json::Value;

use super::context::Context;
use crate::limits::WIDGET_READBACK_MAX_BYTES;

/// The plugin's manifest file.
pub const MANIFEST: &str = "manifest.json";
/// The generated file of a rendered widget, beside its code.
const BUILD_JS: &str = "Build.js";

/// A plugin's manifest at `dir`, read bounded; `None` when it is missing, unreadable or
/// not JSON.
pub fn installed_manifest(dir: &Path) -> Option<Value> {
    serde_json::from_str(&read_small(&dir.join(MANIFEST))?).ok()
}

/// Whether `dir` holds a widget an older omaddons rendered: its manifest's service entry
/// point sits in a plain code folder beside a `Build.js`.
pub fn is_rendered(dir: &Path) -> bool {
    installed_build_js(dir).is_some()
}

/// The `CONTEXT` a rendered widget at `dir` ran its commands under, or `None` when it is
/// no rendered widget or its `Build.js` is not exactly what omaddons wrote.
pub fn installed_context(dir: &Path) -> Option<Context> {
    let text = installed_build_js(dir)?;
    Context::from_json(only_line(&text, "var CONTEXT = ")?)
}

/// The binary a rendered widget at `dir` ran, or `None` as for [`installed_context`].
pub fn installed_binary(dir: &Path) -> Option<PathBuf> {
    let text = installed_build_js(dir)?;
    let value: Value = serde_json::from_str(only_line(&text, "var BINARY = ")?).ok()?;
    value
        .as_str()
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
}

/// The text after `prefix` on the one line that starts with it; `None` when no line or
/// more than one does.
fn only_line<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    let mut found = text.lines().filter_map(|line| line.strip_prefix(prefix));
    let value = found.next()?;
    found.next().is_none().then_some(value)
}

/// `Build.js` beside the service entry point: the manifest names the code folder, which
/// must be one plain component inside `dir`.
fn installed_build_js(dir: &Path) -> Option<String> {
    let manifest = installed_manifest(dir)?;
    let service = Path::new(manifest.get("entryPoints")?.get("service")?.as_str()?);
    let folder = service.parent()?;
    let plain = folder
        .components()
        .all(|c| matches!(c, Component::Normal(_)));
    if !plain || folder.components().count() != 1 {
        return None;
    }
    read_small(&dir.join(folder).join(BUILD_JS))
}

/// A small UTF-8 file, read whole; `None` when missing, unreadable, larger than
/// [`WIDGET_READBACK_MAX_BYTES`] or not UTF-8.
fn read_small(path: &Path) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.take(WIDGET_READBACK_MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > WIDGET_READBACK_MAX_BYTES {
        return None;
    }
    String::from_utf8(bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONTEXT: &str = r#"{"HOME":"/r/home","XDG_CACHE_HOME":"/r/cache","XDG_CONFIG_HOME":"/r/config","XDG_DATA_HOME":"/r/data","XDG_STATE_HOME":"/r/state"}"#;

    /// A rendered widget as omaddons 0.2 wrote it: the manifest pointing into `r-<hex>/`,
    /// and `build` as that folder's `Build.js`.
    fn rendered(build: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("r-0123456789abcdef")).unwrap();
        std::fs::write(
            dir.path().join(MANIFEST),
            r#"{"id":"a44.omaddons","entryPoints":{"service":"r-0123456789abcdef/Service.qml","barWidget":"r-0123456789abcdef/BarWidget.qml"}}"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("r-0123456789abcdef").join(BUILD_JS), build).unwrap();
        dir
    }

    fn build_js() -> String {
        format!(
            ".pragma library\n\nvar BINARY = \"/opt/omaddons/bin/omaddons\"\nvar TOOL_VERSION = \"0.2.1\"\nvar PLUGIN_ID = \"a44.omaddons\"\nvar CONTEXT = {CONTEXT}\nvar CACHE_DIR = \"/r/cache/omaddons\"\nvar CONTENT_SHA256 = \"{}\"\n",
            "0".repeat(64)
        )
    }

    #[test]
    fn a_rendered_widget_reads_back() {
        let dir = rendered(&build_js());
        assert!(is_rendered(dir.path()));
        assert_eq!(
            installed_binary(dir.path()),
            Some(PathBuf::from("/opt/omaddons/bin/omaddons"))
        );
        assert_eq!(installed_context(dir.path()), Context::from_json(CONTEXT));
        assert!(installed_context(dir.path()).is_some());
    }

    /// A git checkout of the widget has its entry points at its root and no Build.js.
    #[test]
    fn a_checkout_is_no_rendered_widget() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(MANIFEST),
            r#"{"id":"a44.omaddons","entryPoints":{"service":"Service.qml","barWidget":"BarWidget.qml"}}"#,
        )
        .unwrap();
        std::fs::write(dir.path().join(BUILD_JS), build_js()).unwrap();
        assert!(!is_rendered(dir.path()));
        assert_eq!(installed_context(dir.path()), None);
        assert!(installed_manifest(dir.path()).is_some());
    }

    /// Anything but one line of each reads as nothing.
    #[test]
    fn an_edited_build_js_reads_as_nothing() {
        let original = build_js();
        let twice = format!("{original}var CONTEXT = {CONTEXT}\n");
        let relative = original.replace("\"/r/home\"", "\"relative\"");
        for text in [twice.as_str(), relative.as_str()] {
            assert_eq!(installed_context(rendered(text).path()), None, "{text}");
        }
        let no_binary = original.replace("var BINARY = ", "var BINARIES = ");
        assert_eq!(installed_binary(rendered(&no_binary).path()), None);
        // An escaped folder or an oversized Build.js is none either.
        let dir = rendered(&original);
        std::fs::write(
            dir.path().join(MANIFEST),
            r#"{"entryPoints":{"service":"../r/Service.qml"}}"#,
        )
        .unwrap();
        assert!(!is_rendered(dir.path()));
        let big = rendered(&"x".repeat(WIDGET_READBACK_MAX_BYTES as usize + 1));
        assert!(!is_rendered(big.path()));
    }
}
