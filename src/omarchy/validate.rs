//! A port of Omarchy's `omarchy-plugin-validate` (4.0.6), so a rendered widget is
//! validated whether or not Omarchy's bash script is on the machine. Re-read the script
//! when Omarchy changes its rules. Unlike the script, every violation is reported, not
//! only the first.

use std::path::Path;

use serde_json::Value;

use super::assets::MANIFEST;

/// Each kind and the entry point the shell loads it from; a kind not listed is left
/// alone, as the script leaves it.
const KIND_ENTRY_POINTS: &[(&str, &str)] = &[
    ("bar", "bar"),
    ("bar-widget", "barWidget"),
    ("menu", "menu"),
    ("overlay", "overlay"),
    ("panel", "panel"),
    ("service", "service"),
];

/// Every rule the plugin at `dir` breaks, one line each; empty when it is valid.
pub fn validate(dir: &Path) -> Vec<String> {
    let mut problems = Vec::new();
    if !dir.is_dir() {
        return vec![format!("plugin folder not found: {}", dir.display())];
    }
    match std::fs::read_to_string(dir.join(MANIFEST)) {
        Err(_) => problems.push("missing manifest.json".to_owned()),
        Ok(text) => match serde_json::from_str::<Value>(&text) {
            Err(_) => problems.push("manifest.json is not valid JSON".to_owned()),
            Ok(manifest) => check_manifest(dir, &manifest, &mut problems),
        },
    }
    if let Some(link) = first_symlink(dir) {
        problems.push(format!(
            "symlinks are not allowed inside a plugin folder: {}",
            link.display()
        ));
    }
    problems
}

fn check_manifest(dir: &Path, manifest: &Value, problems: &mut Vec<String>) {
    // jq's `==` is type-aware: the string "1" is not the number 1.
    if manifest.get("schemaVersion").and_then(Value::as_f64) != Some(1.0) {
        problems.push("unsupported or missing schemaVersion (expected 1)".to_owned());
    }
    for field in ["id", "name", "version", "kinds", "entryPoints"] {
        if manifest.get(field).is_none() {
            problems.push(format!("manifest missing required field '{field}'"));
        }
    }
    let id = manifest.get("id").and_then(Value::as_str).unwrap_or("");
    if id.is_empty() {
        problems.push("manifest 'id' is empty".to_owned());
    } else if !valid_id(id) || id.contains("..") {
        problems.push(format!("invalid plugin id '{id}'"));
    } else if id.starts_with("omarchy.") {
        problems.push(format!(
            "plugin id '{id}' uses the reserved omarchy.* namespace"
        ));
    }
    let kinds = manifest.get("kinds").and_then(Value::as_array);
    if kinds.is_none_or(Vec::is_empty) {
        problems.push("'kinds' must be a non-empty array".to_owned());
    }
    let entry_points = manifest.get("entryPoints").and_then(Value::as_object);
    if entry_points.is_none() {
        problems.push("'entryPoints' must be an object".to_owned());
    }
    let section = manifest
        .get("barWidget")
        .and_then(Value::as_object)
        .and_then(|bar_widget| bar_widget.get("defaultSection"));
    if section.is_some_and(|s| !matches!(s.as_str(), Some("left" | "center" | "right"))) {
        problems.push("'barWidget.defaultSection' must be left, center, or right".to_owned());
    }
    for value in entry_points.into_iter().flat_map(|m| m.values()) {
        // The script reads each value as text, so a non-string is its JSON.
        let entry = value
            .as_str()
            .map_or_else(|| value.to_string(), str::to_owned);
        if entry.is_empty() {
            problems.push("entry point path is empty".to_owned());
        } else if entry.contains('\n') {
            problems.push("entry point may not contain a newline".to_owned());
        } else if entry.starts_with('/') {
            problems.push(format!("entry point must be a relative path: '{entry}'"));
        } else if entry.contains("..") {
            problems.push(format!("entry point may not contain '..': '{entry}'"));
        } else if !dir.join(&entry).is_file() {
            problems.push(format!("entry point file not found: '{entry}'"));
        }
    }
    for (kind, entry_point) in KIND_ENTRY_POINTS {
        let declared = kinds.is_some_and(|k| k.iter().any(|v| v.as_str() == Some(kind)));
        if declared && !entry_points.is_some_and(|m| m.contains_key(*entry_point)) {
            problems.push(format!(
                "kind '{kind}' requires an 'entryPoints.{entry_point}' to load"
            ));
        }
    }
}

/// `^[A-Za-z0-9][A-Za-z0-9._-]*$`
fn valid_id(id: &str) -> bool {
    let mut bytes = id.bytes();
    bytes.next().is_some_and(|b| b.is_ascii_alphanumeric())
        && bytes.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// The first symlink under `dir`, never following one, skipping `.git` as the script
/// does.
fn first_symlink(dir: &Path) -> Option<std::path::PathBuf> {
    let mut pending = vec![dir.to_path_buf()];
    while let Some(current) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.file_type().is_symlink() {
                return Some(path);
            }
            if meta.is_dir() && entry.file_name() != ".git" {
                pending.push(path);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::json;

    use super::*;
    use crate::omarchy::render::{Context, render};

    fn rendered() -> tempfile::TempDir {
        let ctx = Context {
            home: "/h".into(),
            xdg_config_home: "/h/.config".into(),
            xdg_state_home: "/h/.local/state".into(),
            xdg_cache_home: "/h/.cache".into(),
            xdg_data_home: "/h/.local/share".into(),
        };
        let r = render(std::path::Path::new("/o/omaddons"), &ctx, "a44.omaddons").unwrap();
        let dir = tempfile::tempdir().unwrap();
        for (path, bytes) in &r.files {
            let target = dir.path().join(path);
            fs::create_dir_all(target.parent().unwrap()).unwrap();
            fs::write(target, bytes).unwrap();
        }
        dir
    }

    /// A small valid plugin with a service and a bar widget.
    fn plugin(manifest: &Value) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("Service.qml"), "Item {}\n").unwrap();
        fs::write(dir.path().join("BarWidget.qml"), "Item {}\n").unwrap();
        fs::write(dir.path().join("manifest.json"), manifest.to_string()).unwrap();
        dir
    }

    fn valid() -> Value {
        json!({
            "schemaVersion": 1,
            "id": "a44.example",
            "name": "example",
            "version": "1.0.0",
            "kinds": ["service", "bar-widget"],
            "entryPoints": { "service": "Service.qml", "barWidget": "BarWidget.qml" },
            "barWidget": { "defaultSection": "right" }
        })
    }

    #[test]
    fn the_rendered_widget_is_valid() {
        let dir = rendered();
        assert_eq!(validate(dir.path()), Vec::<String>::new());
    }

    #[test]
    fn a_plain_valid_plugin_is_valid() {
        assert_eq!(validate(plugin(&valid()).path()), Vec::<String>::new());
    }

    /// A case: what is planted, how, and what the report says.
    type Plant = (&'static str, Box<dyn Fn(&mut Value)>, &'static str);

    /// One planted violation at a time, each reported.
    #[test]
    fn every_rule_bites() {
        let cases: Vec<Plant> = vec![
            (
                "string schemaVersion",
                Box::new(|m| m["schemaVersion"] = json!("1")),
                "schemaVersion",
            ),
            (
                "missing name",
                Box::new(|m| {
                    m.as_object_mut().unwrap().remove("name");
                }),
                "required field 'name'",
            ),
            (
                "empty id",
                Box::new(|m| m["id"] = json!("")),
                "'id' is empty",
            ),
            (
                "bad id",
                Box::new(|m| m["id"] = json!("-bad")),
                "invalid plugin id",
            ),
            (
                "dotted id",
                Box::new(|m| m["id"] = json!("a..b")),
                "invalid plugin id",
            ),
            (
                "reserved id",
                Box::new(|m| m["id"] = json!("omarchy.mine")),
                "reserved omarchy.*",
            ),
            (
                "empty kinds",
                Box::new(|m| m["kinds"] = json!([])),
                "non-empty array",
            ),
            (
                "entryPoints not an object",
                Box::new(|m| m["entryPoints"] = json!([])),
                "must be an object",
            ),
            (
                "bad section",
                Box::new(|m| m["barWidget"]["defaultSection"] = json!("top")),
                "defaultSection",
            ),
            (
                "dot-dot entry",
                Box::new(|m| m["entryPoints"]["service"] = json!("../Service.qml")),
                "may not contain '..'",
            ),
            (
                "absolute entry",
                Box::new(|m| m["entryPoints"]["service"] = json!("/etc/passwd")),
                "relative path",
            ),
            (
                "empty entry",
                Box::new(|m| m["entryPoints"]["service"] = json!("")),
                "is empty",
            ),
            (
                "newline entry",
                Box::new(|m| m["entryPoints"]["service"] = json!("Service\n.qml")),
                "newline",
            ),
            (
                "missing entry file",
                Box::new(|m| m["entryPoints"]["service"] = json!("Gone.qml")),
                "not found",
            ),
            (
                "kind without entry",
                Box::new(|m| {
                    m["entryPoints"]
                        .as_object_mut()
                        .unwrap()
                        .remove("barWidget");
                }),
                "kind 'bar-widget' requires",
            ),
        ];
        for (what, plant, expected) in cases {
            let mut manifest = valid();
            plant(&mut manifest);
            let problems = validate(plugin(&manifest).path());
            assert!(
                problems.iter().any(|p| p.contains(expected)),
                "{what}: {problems:?}"
            );
        }
    }

    #[test]
    fn a_symlink_anywhere_is_refused_but_git_is_skipped() {
        let dir = plugin(&valid());
        fs::create_dir_all(dir.path().join("views/deep")).unwrap();
        std::os::unix::fs::symlink("/etc/passwd", dir.path().join("views/deep/link")).unwrap();
        assert!(
            validate(dir.path())
                .iter()
                .any(|p| p.contains("symlinks are not allowed")),
            "{:?}",
            validate(dir.path())
        );
        let dir = plugin(&valid());
        fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::os::unix::fs::symlink("/etc/passwd", dir.path().join(".git/link")).unwrap();
        assert_eq!(validate(dir.path()), Vec::<String>::new());
    }

    #[test]
    fn a_missing_or_broken_manifest_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            validate(dir.path()),
            vec!["missing manifest.json".to_owned()]
        );
        fs::write(dir.path().join("manifest.json"), "{").unwrap();
        assert_eq!(
            validate(dir.path()),
            vec!["manifest.json is not valid JSON".to_owned()]
        );
        assert!(validate(&dir.path().join("nope"))[0].starts_with("plugin folder not found"));
    }
}
