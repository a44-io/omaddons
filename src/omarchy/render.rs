//! One install's widget (M3-D7): the embedded files with the manifest's `version` and
//! `id` stamped, and a generated `Build.js` holding the installing binary, the tool
//! version, the plugin id and the data context every widget command must run under.
//!
//! The code lives in a folder named for a hash of its bytes, `Build.js` included
//! (`r-<16 hex>/`), and the manifest's entry points are stamped to point into it.
//! Omarchy's hot reload never recompiles code whose file URL is unchanged (F0), so a
//! reinstall that changes any byte, the binary path included, must put the code at new
//! URLs. `content_sha256` is computed over the logical files before that folder exists,
//! so a moved binary changes the folder and `Build.js` but never the hash `status`
//! compares ("outdated" means other code or another context, not another path).

use std::io::Read;
use std::path::{Component, Path, PathBuf};

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use super::assets::{ASSETS, MANIFEST};
use crate::error::{Error, ErrorCode, Result, Suggestion};
use crate::limits::WIDGET_READBACK_MAX_BYTES;
use crate::state::paths::Paths;

/// The generated file, beside the code it configures.
pub const BUILD_JS: &str = "Build.js";
/// The code folder: this prefix and the first [`CODE_HASH_HEX`] hex digits of the
/// code's hash.
const CODE_DIR_PREFIX: &str = "r-";
const CODE_HASH_HEX: usize = 16;

/// The installing run's effective roots: exactly the inputs `Paths::from_env` reads,
/// defaults written out, so a child given them resolves the same `<config>`, `<state>`
/// and `<cache>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Context {
    pub home: PathBuf,
    pub xdg_config_home: PathBuf,
    pub xdg_state_home: PathBuf,
    pub xdg_cache_home: PathBuf,
    pub xdg_data_home: PathBuf,
}

impl Context {
    /// The roots `paths` was resolved from.
    pub fn of(paths: &Paths) -> Self {
        Self {
            home: paths.home().to_path_buf(),
            xdg_config_home: paths.config_home().to_path_buf(),
            xdg_state_home: paths.state_home().to_path_buf(),
            xdg_cache_home: paths.cache_home().to_path_buf(),
            xdg_data_home: paths.data_home().to_path_buf(),
        }
    }

    /// The five variables a widget child is given.
    pub fn as_env(&self) -> [(&'static str, &Path); 5] {
        [
            ("HOME", &self.home),
            ("XDG_CONFIG_HOME", &self.xdg_config_home),
            ("XDG_STATE_HOME", &self.xdg_state_home),
            ("XDG_CACHE_HOME", &self.xdg_cache_home),
            ("XDG_DATA_HOME", &self.xdg_data_home),
        ]
    }

    /// `<cache>/omaddons`, the directory the doorbell rings in.
    pub fn cache_dir(&self) -> PathBuf {
        Paths::with_roots(
            &self.home,
            &self.xdg_config_home,
            &self.xdg_state_home,
            &self.xdg_cache_home,
        )
        .cache_dir()
        .to_path_buf()
    }

    /// `CONTEXT`'s JSON: one line, keys sorted.
    fn to_json(&self) -> Result<String> {
        let mut map = Map::new();
        for (name, path) in self.as_env() {
            map.insert(name.to_owned(), Value::String(utf8(path)?.to_owned()));
        }
        Ok(js_safe(&Value::Object(map).to_string()))
    }

    /// The context in `CONTEXT`'s JSON: exactly the five names, absolute paths.
    fn from_json(text: &str) -> Option<Self> {
        let Value::Object(map) = serde_json::from_str::<Value>(text).ok()? else {
            return None;
        };
        if map.len() != 5 {
            return None;
        }
        let get = |name: &str| {
            map.get(name)
                .and_then(Value::as_str)
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
        };
        Some(Self {
            home: get("HOME")?,
            xdg_config_home: get("XDG_CONFIG_HOME")?,
            xdg_state_home: get("XDG_STATE_HOME")?,
            xdg_cache_home: get("XDG_CACHE_HOME")?,
            xdg_data_home: get("XDG_DATA_HOME")?,
        })
    }
}

/// The rendered plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rendered {
    /// `(path within the plugin, bytes)`, sorted by path: `manifest.json` and the code
    /// under [`Rendered::code_dir`].
    pub files: Vec<(String, Vec<u8>)>,
    /// What `status` compares to call a widget outdated: the context, the tool version
    /// and every file but `Build.js`, before the code folder is applied.
    pub content_sha256: String,
    /// `r-<16 hex>`: the code's folder, named for its bytes.
    pub code_dir: String,
}

/// The widget for `plugin_id`, run by `binary` under `ctx`. A path that is not UTF-8
/// cannot be written into `Build.js` and is refused.
pub fn render(binary: &Path, ctx: &Context, plugin_id: &str) -> Result<Rendered> {
    let version = env!("CARGO_PKG_VERSION");
    let mut manifest = manifest_asset()?;
    if let Value::Object(fields) = &mut manifest {
        fields.insert("version".to_owned(), Value::String(version.to_owned()));
        fields.insert("id".to_owned(), Value::String(plugin_id.to_owned()));
    }
    let mut logical: Vec<(String, Vec<u8>)> = ASSETS
        .iter()
        .filter(|(path, _)| *path != MANIFEST)
        .map(|(path, text)| ((*path).to_owned(), text.as_bytes().to_vec()))
        .collect();
    logical.push((MANIFEST.to_owned(), pretty(&manifest)?));
    logical.sort();

    let mut content = Fields::new("omaddons widget content 1");
    content.field(version.as_bytes());
    for (_, path) in ctx.as_env() {
        content.field(utf8(path)?.as_bytes());
    }
    for (path, bytes) in &logical {
        content.field(path.as_bytes());
        content.field(bytes);
    }
    let content_sha256 = content.hex();

    let build_js = build_js(binary, ctx, plugin_id, &content_sha256)?;
    let mut code: Vec<(String, Vec<u8>)> = logical
        .into_iter()
        .filter(|(path, _)| path != MANIFEST)
        .collect();
    code.push((BUILD_JS.to_owned(), build_js.into_bytes()));
    code.sort();
    let mut code_hash = Fields::new("omaddons widget code 1");
    for (path, bytes) in &code {
        code_hash.field(path.as_bytes());
        code_hash.field(bytes);
    }
    let code_dir = format!("{CODE_DIR_PREFIX}{}", &code_hash.hex()[..CODE_HASH_HEX]);

    if let Some(Value::Object(entry_points)) = manifest.get_mut("entryPoints") {
        for value in entry_points.values_mut() {
            if let Value::String(path) = value {
                *path = format!("{code_dir}/{path}");
            }
        }
    }
    let mut files = vec![(MANIFEST.to_owned(), pretty(&manifest)?)];
    files.extend(
        code.into_iter()
            .map(|(path, bytes)| (format!("{code_dir}/{path}"), bytes)),
    );
    files.sort();
    Ok(Rendered {
        files,
        content_sha256,
        code_dir,
    })
}

/// The embedded manifest, parsed.
fn manifest_asset() -> Result<Value> {
    let text = ASSETS
        .iter()
        .find(|(path, _)| *path == MANIFEST)
        .map(|(_, text)| *text)
        .ok_or_else(|| Error::internal("the widget's manifest.json is not embedded"))?;
    serde_json::from_str(text)
        .map_err(|e| Error::internal(format!("the widget's manifest.json is not JSON: {e}")))
}

/// Pretty JSON with a trailing newline.
fn pretty(value: &Value) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec_pretty(value)
        .map_err(|e| Error::internal(format!("rendering the widget's manifest: {e}")))?;
    bytes.push(b'\n');
    Ok(bytes)
}

/// `Build.js`: one `var` per line, every value a JSON literal.
fn build_js(binary: &Path, ctx: &Context, plugin_id: &str, content_sha256: &str) -> Result<String> {
    let string = |s: &str| js_safe(&Value::String(s.to_owned()).to_string());
    let cache_dir = ctx.cache_dir();
    Ok(format!(
        ".pragma library\n\
         \n\
         // Generated by omaddons at install; never edited, never committed.\n\
         var BINARY = {}\n\
         var TOOL_VERSION = {}\n\
         var PLUGIN_ID = {}\n\
         var CONTEXT = {}\n\
         var CACHE_DIR = {}\n\
         var CONTENT_SHA256 = \"{content_sha256}\"\n",
        string(utf8(binary)?),
        string(env!("CARGO_PKG_VERSION")),
        string(plugin_id),
        ctx.to_json()?,
        string(utf8(&cache_dir)?),
    ))
}

/// JSON text made safe inside JavaScript source: U+2028 and U+2029 end a line there,
/// though not in JSON.
fn js_safe(json: &str) -> String {
    json.replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029")
}

fn utf8(path: &Path) -> Result<&str> {
    path.to_str().ok_or_else(|| {
        Error::new(
            ErrorCode::Usage,
            format!(
                "{} is not valid UTF-8, so the widget cannot record it",
                path.display()
            ),
            Suggestion::text(
                "install the widget from a binary path, HOME and XDG directories that are valid UTF-8",
            ),
        )
    })
}

/// A SHA-256 over length-prefixed fields, so no two inputs collide by concatenation.
struct Fields(Sha256);

impl Fields {
    fn new(domain: &str) -> Self {
        let mut fields = Self(Sha256::new());
        fields.field(domain.as_bytes());
        fields
    }

    fn field(&mut self, bytes: &[u8]) {
        self.0.update((bytes.len() as u64).to_be_bytes());
        self.0.update(bytes);
    }

    fn hex(self) -> String {
        self.0
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }
}

// ---------------------------------------------------------------------------------------
// Reading an installed widget back.

/// The `CONTENT_SHA256` an installed widget at `dir` was rendered with, or `None` when
/// its manifest or `Build.js` is missing or is not exactly what omaddons writes.
pub fn installed_sha256(dir: &Path) -> Option<String> {
    let text = installed_build_js(dir)?;
    let value = only_line(&text, "var CONTENT_SHA256 = ")?;
    let hex = value.strip_prefix('"')?.strip_suffix('"')?;
    (hex.len() == 64 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
        .then(|| hex.to_owned())
}

/// The `CONTEXT` an installed widget at `dir` runs its commands under, or `None` as for
/// [`installed_sha256`].
pub fn installed_context(dir: &Path) -> Option<Context> {
    let text = installed_build_js(dir)?;
    Context::from_json(only_line(&text, "var CONTEXT = ")?)
}

/// The text after `prefix` on the one line that starts with it; `None` when no line or
/// more than one does.
fn only_line<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    let mut found = text.lines().filter_map(|line| line.strip_prefix(prefix));
    let value = found.next()?;
    found.next().is_none().then_some(value)
}

/// `Build.js` beside the installed service entry point: the manifest names the code
/// folder, which must be one plain component inside `dir`.
fn installed_build_js(dir: &Path) -> Option<String> {
    let manifest: Value = serde_json::from_str(&read_small(&dir.join(MANIFEST))?).ok()?;
    let service = Path::new(manifest.get("entryPoints")?.get("service")?.as_str()?);
    let folder = service.parent()?;
    let plain = folder
        .components()
        .all(|c| matches!(c, Component::Normal(_)));
    if !plain || folder.components().count() > 1 {
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
    use std::collections::BTreeMap;
    use std::ffi::OsString;

    use super::*;

    fn ctx(root: &str) -> Context {
        Context {
            home: PathBuf::from(format!("{root}/home")),
            xdg_config_home: PathBuf::from(format!("{root}/config")),
            xdg_state_home: PathBuf::from(format!("{root}/state")),
            xdg_cache_home: PathBuf::from(format!("{root}/cache")),
            xdg_data_home: PathBuf::from(format!("{root}/data")),
        }
    }

    fn binary() -> PathBuf {
        PathBuf::from("/opt/omaddons/bin/omaddons")
    }

    fn file<'a>(rendered: &'a Rendered, suffix: &str) -> &'a [u8] {
        rendered
            .files
            .iter()
            .find(|(path, _)| path.ends_with(suffix))
            .map(|(_, bytes)| bytes.as_slice())
            .unwrap()
    }

    fn written(rendered: &Rendered) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (path, bytes) in &rendered.files {
            let target = dir.path().join(path);
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::write(target, bytes).unwrap();
        }
        dir
    }

    #[test]
    fn rendering_is_deterministic() {
        let a = render(&binary(), &ctx("/r"), "a44.omaddons").unwrap();
        let b = render(&binary(), &ctx("/r"), "a44.omaddons").unwrap();
        assert_eq!(a, b);
        assert_eq!(a.content_sha256.len(), 64);
        assert!(
            a.code_dir.starts_with("r-") && a.code_dir.len() == 18,
            "{}",
            a.code_dir
        );
    }

    /// Every runtime file is under the code folder, the manifest at the root, its entry
    /// points stamped into the folder.
    #[test]
    fn the_code_lives_in_its_folder_and_the_manifest_points_there() {
        let r = render(&binary(), &ctx("/r"), "a44.omaddons").unwrap();
        let paths: Vec<&str> = r.files.iter().map(|(p, _)| p.as_str()).collect();
        assert!(paths.contains(&"manifest.json"));
        for (asset, _) in ASSETS.iter().filter(|(p, _)| *p != MANIFEST) {
            assert!(
                paths.contains(&format!("{}/{asset}", r.code_dir).as_str()),
                "{asset}"
            );
        }
        assert!(paths.contains(&format!("{}/Build.js", r.code_dir).as_str()));
        assert_eq!(paths.len(), ASSETS.len() + 1);
        let manifest: Value = serde_json::from_slice(file(&r, "manifest.json")).unwrap();
        assert_eq!(
            manifest["entryPoints"]["service"],
            format!("{}/Service.qml", r.code_dir)
        );
        assert_eq!(
            manifest["entryPoints"]["barWidget"],
            format!("{}/BarWidget.qml", r.code_dir)
        );
    }

    /// The manifest is stamped through a structured edit, stays pretty JSON with a
    /// trailing newline, and keeps everything else.
    #[test]
    fn the_manifest_carries_the_tool_version_and_the_id() {
        let r = render(&binary(), &ctx("/r"), "a44.omaddons-checkpoint").unwrap();
        let bytes = file(&r, "manifest.json");
        assert!(bytes.ends_with(b"}\n"));
        assert!(
            std::str::from_utf8(bytes)
                .unwrap()
                .contains("\n  \"schemaVersion\": 1,")
        );
        let manifest: Value = serde_json::from_slice(bytes).unwrap();
        assert_eq!(manifest["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(manifest["id"], "a44.omaddons-checkpoint");
        let embedded: Value =
            serde_json::from_str(ASSETS.iter().find(|(p, _)| *p == MANIFEST).unwrap().1).unwrap();
        assert_eq!(manifest["barWidget"], embedded["barWidget"]);
        let build = std::str::from_utf8(file(&r, "Build.js")).unwrap();
        assert!(
            build.contains("\nvar PLUGIN_ID = \"a44.omaddons-checkpoint\"\n"),
            "{build}"
        );
        assert!(build.contains(&format!(
            "\nvar TOOL_VERSION = \"{}\"\n",
            env!("CARGO_PKG_VERSION")
        )));
    }

    /// Another context is another `CONTEXT` and another hash.
    #[test]
    fn another_context_changes_context_and_the_hash() {
        let a = render(&binary(), &ctx("/r"), "a44.omaddons").unwrap();
        let b = render(&binary(), &ctx("/other"), "a44.omaddons").unwrap();
        assert_ne!(a.content_sha256, b.content_sha256);
        assert_ne!(file(&a, "Build.js"), file(&b, "Build.js"));
        let build = std::str::from_utf8(file(&b, "Build.js")).unwrap();
        assert!(
            build.contains("\nvar CACHE_DIR = \"/other/cache/omaddons\"\n"),
            "{build}"
        );
    }

    /// Another binary path is another `BINARY`, so another code folder (new URLs, which
    /// Omarchy's hot reload needs), but the same code and the same hash.
    #[test]
    fn another_binary_changes_only_build_js_and_the_folder() {
        let a = render(&binary(), &ctx("/r"), "a44.omaddons").unwrap();
        let b = render(Path::new("/elsewhere/omaddons"), &ctx("/r"), "a44.omaddons").unwrap();
        assert_eq!(a.content_sha256, b.content_sha256);
        assert_ne!(a.code_dir, b.code_dir);
        let strip = |r: &Rendered| -> BTreeMap<String, Vec<u8>> {
            r.files
                .iter()
                .map(|(p, bytes)| (p.replace(&r.code_dir, "CODE"), bytes.clone()))
                .collect()
        };
        let (left, right) = (strip(&a), strip(&b));
        for (path, bytes) in &left {
            if path.ends_with("Build.js") || path == "manifest.json" {
                continue;
            }
            assert_eq!(Some(bytes), right.get(path), "{path}");
        }
        let lines = |r: &Rendered| -> Vec<String> {
            std::str::from_utf8(file(r, "Build.js"))
                .unwrap()
                .lines()
                .map(str::to_owned)
                .collect()
        };
        let differing: Vec<(String, String)> = lines(&a)
            .into_iter()
            .zip(lines(&b))
            .filter(|(x, y)| x != y)
            .collect();
        assert_eq!(differing.len(), 1, "{differing:?}");
        assert!(differing[0].0.starts_with("var BINARY = "));
        let manifest = |r: &Rendered| -> Value {
            let mut m: Value = serde_json::from_slice(file(r, "manifest.json")).unwrap();
            m["entryPoints"] = Value::Null;
            m
        };
        assert_eq!(manifest(&a), manifest(&b));
    }

    /// Quotes, backslashes, non-ASCII and JavaScript line separators in a path stay
    /// inside their literal, and read back exactly.
    #[test]
    fn hostile_paths_are_escaped_and_read_back() {
        let mut hostile = ctx("/r");
        hostile.home = PathBuf::from("/r/h\"o\\me \u{e9}\u{2028}x\n");
        hostile.xdg_cache_home = PathBuf::from("/r/ca\"che");
        let r = render(Path::new("/opt/o\"m/omaddons"), &hostile, "a44.omaddons").unwrap();
        let build = std::str::from_utf8(file(&r, "Build.js")).unwrap();
        assert!(!build.contains('\u{2028}'));
        assert_eq!(
            build.lines().filter(|l| l.starts_with("var ")).count(),
            6,
            "{build}"
        );
        let dir = written(&r);
        assert_eq!(installed_context(dir.path()), Some(hostile));
        assert_eq!(installed_sha256(dir.path()), Some(r.content_sha256.clone()));
    }

    #[test]
    fn a_path_that_is_not_utf8_is_refused() {
        use std::os::unix::ffi::OsStringExt;
        let mut bad = ctx("/r");
        bad.home = PathBuf::from(OsString::from_vec(b"/r/h\xffme".to_vec()));
        let err = render(&binary(), &bad, "a44.omaddons").unwrap_err();
        assert_eq!(err.code, ErrorCode::Usage);
        let err = render(
            Path::new(&OsString::from_vec(b"/o\xff".to_vec())),
            &ctx("/r"),
            "a44.omaddons",
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::Usage);
    }

    #[test]
    fn an_installed_widget_reads_back() {
        let r = render(&binary(), &ctx("/r"), "a44.omaddons").unwrap();
        let dir = written(&r);
        assert_eq!(installed_sha256(dir.path()), Some(r.content_sha256.clone()));
        assert_eq!(installed_context(dir.path()), Some(ctx("/r")));
    }

    /// Anything but exactly what omaddons writes reads back as nothing.
    #[test]
    fn a_missing_truncated_or_edited_build_js_reads_as_nothing() {
        let r = render(&binary(), &ctx("/r"), "a44.omaddons").unwrap();
        let build_path = |dir: &Path| dir.join(&r.code_dir).join(BUILD_JS);
        let original = String::from_utf8(file(&r, "Build.js").to_vec()).unwrap();
        let sha = &r.content_sha256;
        let edits: Vec<(&str, String)> = vec![
            ("truncated", original[..original.len() - 20].to_owned()),
            (
                "extra space",
                original.replace(
                    &format!("var CONTENT_SHA256 = \"{sha}\""),
                    &format!("var CONTENT_SHA256 =  \"{sha}\""),
                ),
            ),
            ("short hex", original.replace(sha.as_str(), &sha[..63])),
            (
                "upper hex",
                original.replace(sha.as_str(), &sha.to_uppercase()),
            ),
            (
                "a second line",
                format!("{original}var CONTENT_SHA256 = \"{sha}\"\n"),
            ),
        ];
        for (what, text) in edits {
            let dir = written(&r);
            std::fs::write(build_path(dir.path()), text).unwrap();
            assert_eq!(installed_sha256(dir.path()), None, "{what}");
        }
        // A CONTEXT with a relative path, a missing name or an extra one.
        for bad in [
            r#"var CONTEXT = {"HOME":"relative","XDG_CACHE_HOME":"/c","XDG_CONFIG_HOME":"/c","XDG_DATA_HOME":"/d","XDG_STATE_HOME":"/s"}"#,
            r#"var CONTEXT = {"XDG_CACHE_HOME":"/c","XDG_CONFIG_HOME":"/c","XDG_DATA_HOME":"/d","XDG_STATE_HOME":"/s"}"#,
            r#"var CONTEXT = {"EXTRA":"/e","HOME":"/h","XDG_CACHE_HOME":"/c","XDG_CONFIG_HOME":"/c","XDG_DATA_HOME":"/d","XDG_STATE_HOME":"/s"}"#,
        ] {
            let dir = written(&r);
            let edited = original
                .lines()
                .map(|l| {
                    if l.starts_with("var CONTEXT = ") {
                        bad
                    } else {
                        l
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            std::fs::write(build_path(dir.path()), edited).unwrap();
            assert_eq!(installed_context(dir.path()), None, "{bad}");
        }
        let dir = written(&r);
        std::fs::remove_file(build_path(dir.path())).unwrap();
        assert_eq!(installed_sha256(dir.path()), None);
        // A manifest pointing outside the plugin.
        let dir = written(&r);
        let mut manifest: Value =
            serde_json::from_slice(&std::fs::read(dir.path().join(MANIFEST)).unwrap()).unwrap();
        manifest["entryPoints"]["service"] = Value::String("../elsewhere/Service.qml".into());
        std::fs::write(dir.path().join(MANIFEST), manifest.to_string()).unwrap();
        assert_eq!(installed_sha256(dir.path()), None);
        // An oversized Build.js.
        let dir = written(&r);
        std::fs::write(
            build_path(dir.path()),
            format!(
                "{original}//{}",
                "x".repeat(WIDGET_READBACK_MAX_BYTES as usize)
            ),
        )
        .unwrap();
        assert_eq!(installed_sha256(dir.path()), None);
    }

    /// A child given `CONTEXT` resolves the very same paths, defaults and fallbacks
    /// included.
    #[test]
    fn paths_from_a_rendered_context_are_the_installing_paths() {
        let lookup = |vars: Vec<(&str, OsString)>| {
            let vars: BTreeMap<String, OsString> =
                vars.into_iter().map(|(k, v)| (k.to_owned(), v)).collect();
            move |name: &str| vars.get(name).cloned()
        };
        for vars in [
            vec![("HOME", "/h".into())],
            vec![
                ("HOME", "/h".into()),
                ("XDG_CONFIG_HOME", "/xc".into()),
                ("XDG_STATE_HOME", "/xs".into()),
                ("XDG_CACHE_HOME", "/xk".into()),
                ("XDG_DATA_HOME", "/xd".into()),
            ],
            vec![("HOME", "/h".into()), ("XDG_STATE_HOME", "relative".into())],
        ] {
            let p = Paths::from_lookup(lookup(vars.clone())).unwrap();
            let context = Context::of(&p);
            let env: Vec<(&str, OsString)> = context
                .as_env()
                .iter()
                .map(|(name, path)| (*name, path.as_os_str().to_owned()))
                .collect();
            assert_eq!(Paths::from_lookup(lookup(env)).unwrap(), p, "{vars:?}");
            // And what Build.js records is what reads back.
            let r = render(&binary(), &context, "a44.omaddons").unwrap();
            assert_eq!(installed_context(written(&r).path()), Some(context));
        }
    }
}
