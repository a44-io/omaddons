//! A port of Omarchy's `omarchy-plugin-validate` (4.0.6), so a rendered widget is
//! validated whether or not Omarchy's bash script is on the machine. Re-read the script
//! when Omarchy changes its rules. Unlike the script, every violation is reported, not
//! only the first, each with its [`Rule`] code.
//!
//! One rule is the port's own: `manifest.json` is read with at most
//! [`WIDGET_READBACK_MAX_BYTES`] (64 KiB), and a larger one is [`Rule::ManifestTooLarge`],
//! never parsed. The installed folder is untrusted input (common rule 15), and the real
//! widget's manifest is under 1 KiB.

use std::io::Read;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::Serialize;
use serde_json::Value;

use super::assets::MANIFEST;
use crate::limits::WIDGET_READBACK_MAX_BYTES;

/// The rule a problem breaks: one per message the port reports. Declared in the order of
/// their wire codes, so sorting rules sorts their codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Rule {
    /// `barWidget.defaultSection` is not left, center or right.
    DefaultSection,
    /// An entry point is an absolute path.
    EntryPointAbsolute,
    /// An entry point holds `..`.
    EntryPointDotdot,
    /// An entry point is empty.
    EntryPointEmpty,
    /// An entry point names no file.
    EntryPointMissing,
    /// An entry point holds a newline.
    EntryPointNewline,
    /// `entryPoints` is not an object.
    EntryPointsNotObject,
    /// A required field is missing.
    FieldMissing,
    /// `id` is empty.
    IdEmpty,
    /// `id` breaks the id pattern or holds `..`.
    IdInvalid,
    /// `id` is in Omarchy's reserved `omarchy.*` namespace.
    IdReserved,
    /// A declared kind has no entry point to load it from.
    KindWithoutEntryPoint,
    /// `kinds` is not a non-empty array.
    KindsEmpty,
    /// `manifest.json` is missing or cannot be read as text.
    ManifestMissing,
    /// `manifest.json` is not JSON.
    ManifestNotJson,
    /// `manifest.json` is larger than 64 KiB (the port's own bound).
    ManifestTooLarge,
    /// The plugin folder does not exist.
    PluginMissing,
    /// `schemaVersion` is missing or not the number 1.
    SchemaVersion,
    /// The plugin folder holds a symlink.
    Symlink,
}

impl Rule {
    /// Every rule, in order.
    pub const ALL: [Self; 19] = [
        Self::DefaultSection,
        Self::EntryPointAbsolute,
        Self::EntryPointDotdot,
        Self::EntryPointEmpty,
        Self::EntryPointMissing,
        Self::EntryPointNewline,
        Self::EntryPointsNotObject,
        Self::FieldMissing,
        Self::IdEmpty,
        Self::IdInvalid,
        Self::IdReserved,
        Self::KindWithoutEntryPoint,
        Self::KindsEmpty,
        Self::ManifestMissing,
        Self::ManifestNotJson,
        Self::ManifestTooLarge,
        Self::PluginMissing,
        Self::SchemaVersion,
        Self::Symlink,
    ];
}

/// One broken rule, and the port's message for it (which may quote manifest values).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub rule: Rule,
    pub message: String,
}

/// What [`check`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checked {
    /// In the order the port checks them.
    pub problems: Vec<Problem>,
    /// `false` when `charge` stopped the symlink walk before it finished.
    pub complete: bool,
}

impl Checked {
    /// The rules broken, sorted by their wire codes, each once.
    pub fn rules(&self) -> Vec<Rule> {
        let mut rules: Vec<Rule> = self.problems.iter().map(|p| p.rule).collect();
        rules.sort_unstable();
        rules.dedup();
        rules
    }
}

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
    check(dir, &mut || true)
        .problems
        .into_iter()
        .map(|problem| problem.message)
        .collect()
}

/// Every rule the plugin at `dir` breaks, each with its code; `charge` is called once per
/// entry the symlink walk visits, and the walk stops (`complete: false`) when it returns
/// false.
pub fn check(dir: &Path, charge: &mut dyn FnMut() -> bool) -> Checked {
    let mut problems = Problems::default();
    if !dir.is_dir() {
        problems.push(
            Rule::PluginMissing,
            format!("plugin folder not found: {}", dir.display()),
        );
        return Checked {
            problems: problems.0,
            complete: true,
        };
    }
    match read_manifest(&dir.join(MANIFEST)) {
        ManifestRead::Missing => problems.push(Rule::ManifestMissing, "missing manifest.json"),
        ManifestRead::TooLarge => problems.push(
            Rule::ManifestTooLarge,
            format!(
                "manifest.json is larger than {} KiB",
                WIDGET_READBACK_MAX_BYTES / 1024
            ),
        ),
        ManifestRead::Text(text) => match serde_json::from_str::<Value>(&text) {
            Err(_) => problems.push(Rule::ManifestNotJson, "manifest.json is not valid JSON"),
            Ok(manifest) => check_manifest(dir, &manifest, &mut problems),
        },
    }
    let (link, complete) = first_symlink(dir, charge);
    if let Some(link) = link {
        problems.push(
            Rule::Symlink,
            format!(
                "symlinks are not allowed inside a plugin folder: {}",
                link.display()
            ),
        );
    }
    Checked {
        problems: problems.0,
        complete,
    }
}

/// The problems found so far, in the order the port checks them.
#[derive(Default)]
struct Problems(Vec<Problem>);

impl Problems {
    fn push(&mut self, rule: Rule, message: impl Into<String>) {
        self.0.push(Problem {
            rule,
            message: message.into(),
        });
    }
}

/// `manifest.json`, read with at most [`WIDGET_READBACK_MAX_BYTES`].
enum ManifestRead {
    /// Missing, unreadable or not UTF-8, as the script's unbounded read reported it.
    Missing,
    TooLarge,
    Text(String),
}

fn read_manifest(path: &Path) -> ManifestRead {
    let Ok(file) = std::fs::File::open(path) else {
        return ManifestRead::Missing;
    };
    let mut bytes = Vec::new();
    if file
        .take(WIDGET_READBACK_MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .is_err()
    {
        return ManifestRead::Missing;
    }
    if bytes.len() as u64 > WIDGET_READBACK_MAX_BYTES {
        return ManifestRead::TooLarge;
    }
    String::from_utf8(bytes).map_or(ManifestRead::Missing, ManifestRead::Text)
}

fn check_manifest(dir: &Path, manifest: &Value, problems: &mut Problems) {
    // jq's `==` is type-aware: the string "1" is not the number 1.
    if manifest.get("schemaVersion").and_then(Value::as_f64) != Some(1.0) {
        problems.push(
            Rule::SchemaVersion,
            "unsupported or missing schemaVersion (expected 1)",
        );
    }
    for field in ["id", "name", "version", "kinds", "entryPoints"] {
        if manifest.get(field).is_none() {
            problems.push(
                Rule::FieldMissing,
                format!("manifest missing required field '{field}'"),
            );
        }
    }
    let id = manifest.get("id").and_then(Value::as_str).unwrap_or("");
    if id.is_empty() {
        problems.push(Rule::IdEmpty, "manifest 'id' is empty");
    } else if !valid_id(id) || id.contains("..") {
        problems.push(Rule::IdInvalid, format!("invalid plugin id '{id}'"));
    } else if id.starts_with("omarchy.") {
        problems.push(
            Rule::IdReserved,
            format!("plugin id '{id}' uses the reserved omarchy.* namespace"),
        );
    }
    let kinds = manifest.get("kinds").and_then(Value::as_array);
    if kinds.is_none_or(Vec::is_empty) {
        problems.push(Rule::KindsEmpty, "'kinds' must be a non-empty array");
    }
    let entry_points = manifest.get("entryPoints").and_then(Value::as_object);
    if entry_points.is_none() {
        problems.push(
            Rule::EntryPointsNotObject,
            "'entryPoints' must be an object",
        );
    }
    let section = manifest
        .get("barWidget")
        .and_then(Value::as_object)
        .and_then(|bar_widget| bar_widget.get("defaultSection"));
    if section.is_some_and(|s| !matches!(s.as_str(), Some("left" | "center" | "right"))) {
        problems.push(
            Rule::DefaultSection,
            "'barWidget.defaultSection' must be left, center, or right",
        );
    }
    for value in entry_points.into_iter().flat_map(|m| m.values()) {
        // The script reads each value as text, so a non-string is its JSON.
        let entry = value
            .as_str()
            .map_or_else(|| value.to_string(), str::to_owned);
        if entry.is_empty() {
            problems.push(Rule::EntryPointEmpty, "entry point path is empty");
        } else if entry.contains('\n') {
            problems.push(
                Rule::EntryPointNewline,
                "entry point may not contain a newline",
            );
        } else if entry.starts_with('/') {
            problems.push(
                Rule::EntryPointAbsolute,
                format!("entry point must be a relative path: '{entry}'"),
            );
        } else if entry.contains("..") {
            problems.push(
                Rule::EntryPointDotdot,
                format!("entry point may not contain '..': '{entry}'"),
            );
        } else if !dir.join(&entry).is_file() {
            problems.push(
                Rule::EntryPointMissing,
                format!("entry point file not found: '{entry}'"),
            );
        }
    }
    for (kind, entry_point) in KIND_ENTRY_POINTS {
        let declared = kinds.is_some_and(|k| k.iter().any(|v| v.as_str() == Some(kind)));
        if declared && !entry_points.is_some_and(|m| m.contains_key(*entry_point)) {
            problems.push(
                Rule::KindWithoutEntryPoint,
                format!("kind '{kind}' requires an 'entryPoints.{entry_point}' to load"),
            );
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
/// does; and whether the walk finished (`false` once `charge`, called before each entry
/// it visits, refused).
fn first_symlink(dir: &Path, charge: &mut dyn FnMut() -> bool) -> (Option<PathBuf>, bool) {
    let mut pending = vec![dir.to_path_buf()];
    while let Some(current) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            if !charge() {
                return (None, false);
            }
            let path = entry.path();
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.file_type().is_symlink() {
                return (Some(path), true);
            }
            if meta.is_dir() && entry.file_name() != ".git" {
                pending.push(path);
            }
        }
    }
    (None, true)
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

    /// A case: what is planted, how, what the report says, and the rule it names.
    type Plant = (&'static str, Box<dyn Fn(&mut Value)>, &'static str, Rule);

    /// One planted violation per case, over [`valid`].
    fn plants() -> Vec<Plant> {
        vec![
            (
                "string schemaVersion",
                Box::new(|m| m["schemaVersion"] = json!("1")),
                "schemaVersion",
                Rule::SchemaVersion,
            ),
            (
                "missing name",
                Box::new(|m| {
                    m.as_object_mut().unwrap().remove("name");
                }),
                "required field 'name'",
                Rule::FieldMissing,
            ),
            (
                "empty id",
                Box::new(|m| m["id"] = json!("")),
                "'id' is empty",
                Rule::IdEmpty,
            ),
            (
                "bad id",
                Box::new(|m| m["id"] = json!("-bad")),
                "invalid plugin id",
                Rule::IdInvalid,
            ),
            (
                "dotted id",
                Box::new(|m| m["id"] = json!("a..b")),
                "invalid plugin id",
                Rule::IdInvalid,
            ),
            (
                "reserved id",
                Box::new(|m| m["id"] = json!("omarchy.mine")),
                "reserved omarchy.*",
                Rule::IdReserved,
            ),
            (
                "empty kinds",
                Box::new(|m| m["kinds"] = json!([])),
                "non-empty array",
                Rule::KindsEmpty,
            ),
            (
                "entryPoints not an object",
                Box::new(|m| m["entryPoints"] = json!([])),
                "must be an object",
                Rule::EntryPointsNotObject,
            ),
            (
                "bad section",
                Box::new(|m| m["barWidget"]["defaultSection"] = json!("top")),
                "defaultSection",
                Rule::DefaultSection,
            ),
            (
                "dot-dot entry",
                Box::new(|m| m["entryPoints"]["service"] = json!("../Service.qml")),
                "may not contain '..'",
                Rule::EntryPointDotdot,
            ),
            (
                "absolute entry",
                Box::new(|m| m["entryPoints"]["service"] = json!("/etc/passwd")),
                "relative path",
                Rule::EntryPointAbsolute,
            ),
            (
                "empty entry",
                Box::new(|m| m["entryPoints"]["service"] = json!("")),
                "is empty",
                Rule::EntryPointEmpty,
            ),
            (
                "newline entry",
                Box::new(|m| m["entryPoints"]["service"] = json!("Service\n.qml")),
                "newline",
                Rule::EntryPointNewline,
            ),
            (
                "missing entry file",
                Box::new(|m| m["entryPoints"]["service"] = json!("Gone.qml")),
                "not found",
                Rule::EntryPointMissing,
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
                Rule::KindWithoutEntryPoint,
            ),
        ]
    }

    /// `Rule::ALL` lists every rule once, in `Ord`'s order, which is their wire codes'
    /// lexicographic order (so `rules()` reads sorted on the wire); the schema's enum is
    /// the same set.
    #[test]
    fn rules_sort_as_their_wire_codes() {
        let codes: Vec<String> = Rule::ALL
            .iter()
            .map(|rule| {
                serde_json::to_value(rule)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect();
        let mut sorted = codes.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(codes, sorted, "declared in code order, each once");
        let mut by_ord = Rule::ALL.to_vec();
        by_ord.sort_unstable();
        assert_eq!(by_ord, Rule::ALL.to_vec(), "Ord is the declaration order");
        let schema = schemars::generate::SchemaSettings::draft2020_12()
            .for_serialize()
            .into_generator()
            .root_schema_for::<Rule>()
            .to_value();
        let mut published: Vec<String> = schema["enum"]
            .as_array()
            .or_else(|| schema["oneOf"].as_array())
            .unwrap()
            .iter()
            .filter_map(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .or_else(|| v["const"].as_str().map(str::to_owned))
            })
            .collect();
        published.sort_unstable();
        assert_eq!(published, sorted, "{schema}");
    }

    /// One planted violation at a time, each reported.
    #[test]
    fn every_rule_bites() {
        for (what, plant, expected, _) in plants() {
            let mut manifest = valid();
            plant(&mut manifest);
            let problems = validate(plugin(&manifest).path());
            assert!(
                problems.iter().any(|p| p.contains(expected)),
                "{what}: {problems:?}"
            );
        }
    }

    /// Never charges.
    fn unbounded() -> impl FnMut() -> bool {
        || true
    }

    /// Each planted violation is reported with its rule, beside the message `validate`
    /// gives for it.
    #[test]
    fn check_names_the_rule_of_every_problem() {
        for (what, plant, expected, rule) in plants() {
            let mut manifest = valid();
            plant(&mut manifest);
            let checked = check(plugin(&manifest).path(), &mut unbounded());
            assert!(checked.complete, "{what}: {checked:?}");
            assert!(
                checked
                    .problems
                    .iter()
                    .any(|p| p.rule == rule && p.message.contains(expected)),
                "{what}: expected {rule:?} with {expected:?}, got {checked:?}"
            );
        }
    }

    /// `validate` is `check`'s messages in order, for every planted violation, the
    /// rendered widget and the broken folders.
    #[test]
    fn validate_is_check_mapped_to_messages() {
        let mut dirs: Vec<(String, tempfile::TempDir)> = plants()
            .into_iter()
            .map(|(what, plant, _, _)| {
                let mut manifest = valid();
                plant(&mut manifest);
                (what.to_owned(), plugin(&manifest))
            })
            .collect();
        dirs.push(("rendered".to_owned(), rendered()));
        let empty = tempfile::tempdir().unwrap();
        let broken = tempfile::tempdir().unwrap();
        fs::write(broken.path().join("manifest.json"), "{").unwrap();
        dirs.push(("no manifest".to_owned(), empty));
        dirs.push(("broken manifest".to_owned(), broken));
        for (what, dir) in &dirs {
            let messages: Vec<String> = check(dir.path(), &mut unbounded())
                .problems
                .into_iter()
                .map(|p| p.message)
                .collect();
            assert_eq!(validate(dir.path()), messages, "{what}");
        }
        let gone = dirs[0].1.path().join("nope");
        let checked = check(&gone, &mut unbounded());
        assert_eq!(
            checked.problems.iter().map(|p| p.rule).collect::<Vec<_>>(),
            vec![Rule::PluginMissing],
            "{checked:?}"
        );
        assert_eq!(
            validate(&gone),
            vec![checked.problems[0].message.clone()],
            "a missing folder"
        );
    }

    #[test]
    fn check_reports_a_symlink_and_skips_git() {
        let dir = plugin(&valid());
        fs::create_dir_all(dir.path().join("views/deep")).unwrap();
        std::os::unix::fs::symlink("/etc/passwd", dir.path().join("views/deep/link")).unwrap();
        let checked = check(dir.path(), &mut unbounded());
        assert_eq!(checked.rules(), vec![Rule::Symlink], "{checked:?}");
        assert!(checked.complete, "{checked:?}");

        let dir = plugin(&valid());
        fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::os::unix::fs::symlink("/etc/passwd", dir.path().join(".git/link")).unwrap();
        let checked = check(dir.path(), &mut unbounded());
        assert_eq!(checked.problems, Vec::new(), "a link under .git");
        assert!(checked.complete);
    }

    /// A manifest over 64 KiB is reported and never parsed; one at the bound is read.
    #[test]
    fn check_bounds_the_manifest_read() {
        let max = usize::try_from(WIDGET_READBACK_MAX_BYTES).unwrap();
        let dir = plugin(&valid());
        // Valid JSON past the bound: parsed, it would pass.
        let mut big = valid();
        big["padding"] = json!("x".repeat(max));
        fs::write(dir.path().join("manifest.json"), big.to_string()).unwrap();
        let checked = check(dir.path(), &mut unbounded());
        assert_eq!(checked.rules(), vec![Rule::ManifestTooLarge], "{checked:?}");
        assert_eq!(
            validate(dir.path()),
            vec!["manifest.json is larger than 64 KiB".to_owned()]
        );

        let mut padded = valid();
        let text = padded.to_string();
        padded["padding"] = json!("x".repeat(max - text.len() - r#","padding":"""#.len()));
        let text = padded.to_string();
        assert_eq!(text.len(), max, "a manifest exactly at the bound");
        fs::write(dir.path().join("manifest.json"), text).unwrap();
        assert_eq!(check(dir.path(), &mut unbounded()).problems, Vec::new());
    }

    /// A refusing charge stops the walk: `complete: false`, no symlink beyond the stop.
    #[test]
    fn check_stops_when_charge_refuses() {
        let dir = plugin(&valid());
        for n in 0..10 {
            fs::create_dir_all(dir.path().join(format!("d{n}"))).unwrap();
        }
        let mut left = 3;
        let mut charge = || {
            if left == 0 {
                return false;
            }
            left -= 1;
            true
        };
        let checked = check(dir.path(), &mut charge);
        assert!(!checked.complete, "{checked:?}");
        assert_eq!(checked.problems, Vec::new(), "the manifest is valid");

        let mut calls = 0;
        let checked = check(dir.path(), &mut || {
            calls += 1;
            true
        });
        assert!(checked.complete);
        // Three files and ten directories.
        assert_eq!(calls, 13, "one charge per entry visited");
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
