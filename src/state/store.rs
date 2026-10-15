//! Schema-versioned JSON state files (vision D8, §5.12, D21).
//!
//! Every persisted format carries its own `schema_version` (`"MAJOR.MINOR"`, compared
//! numerically) as its first key. Loading an older file migrates it forward in memory
//! (the next save writes the current version); a newer file, written by a newer
//! omaddons, is read only if it still decodes and is never written. Saves go through
//! `fsx::mutate`, so they are atomic and, in a transaction session, backed up and
//! logged.
//!
//! Decoding is tolerant: added fields carry `#[serde(default)]`, unknown fields are
//! ignored on read and not preserved on write. A newer file is never rewritten, so
//! dropping unknown fields only ever happens to files this binary's version produced.

use std::cmp::Ordering;
use std::fmt;
use std::path::Path;
use std::str::FromStr;

use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::error::{Error, ErrorCode, Result, Suggestion, Warning, WarningCode};
use crate::fsx::{Content, Op, WriteSession, mutate};

/// A persisted format's version, `"MAJOR.MINOR"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(try_from = "String", into = "String")]
pub struct SchemaVersion {
    pub major: u32,
    pub minor: u32,
}

impl SchemaVersion {
    pub const fn new(major: u32, minor: u32) -> Self {
        Self { major, minor }
    }
}

impl PartialOrd for SchemaVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for SchemaVersion {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor).cmp(&(other.major, other.minor))
    }
}

impl fmt::Display for SchemaVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

impl FromStr for SchemaVersion {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        let invalid = || format!("invalid schema_version {s:?}: expected MAJOR.MINOR");
        let (major, minor) = s.split_once('.').ok_or_else(invalid)?;
        let number = |part: &str| {
            (!part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
                .then(|| part.parse::<u32>().ok())
                .flatten()
                .ok_or_else(invalid)
        };
        Ok(Self::new(number(major)?, number(minor)?))
    }
}

impl TryFrom<String> for SchemaVersion {
    type Error = String;

    fn try_from(value: String) -> std::result::Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<SchemaVersion> for String {
    fn from(version: SchemaVersion) -> Self {
        version.to_string()
    }
}

/// A persisted format.
pub trait Versioned: Serialize + DeserializeOwned + JsonSchema {
    /// The format's name, e.g. `target_record`.
    const KIND: &'static str;
    /// The version this binary writes.
    const CURRENT: SchemaVersion;

    /// Rewrites an older document (`schema_version` already removed) into the current
    /// shape. Pure, so the doctor's `state-version-older` fixer (M4) can reuse it. The
    /// default suits additive minor bumps, whose new fields have serde defaults.
    fn migrate(from: SchemaVersion, raw: serde_json::Value) -> Result<serde_json::Value> {
        let _ = from;
        Ok(raw)
    }
}

/// Every persisted format and its current version, for capabilities (M4) and the
/// wire-name golden. Each `Versioned` impl adds its row here.
pub const KINDS: &[(&str, SchemaVersion)] = &[];

/// How a loaded file's version compared with this binary's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionStatus {
    Current,
    /// Migrated in memory; the next save writes the current version.
    Older,
    /// Written by a newer omaddons; readable, never written.
    Newer,
}

/// A loaded document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Loaded<T> {
    pub value: T,
    /// The version on disk.
    pub version: SchemaVersion,
    pub status: VersionStatus,
}

impl<T: Versioned> Loaded<T> {
    /// The warning a command adds to its outcome for an older or newer file.
    pub fn warning(&self, path: &Path) -> Option<Warning> {
        match self.status {
            VersionStatus::Current => None,
            VersionStatus::Older => Some(Warning::new(
                WarningCode::StateVersionOlder,
                format!(
                    "{} is {} version {}; it will be migrated to {} on the next write",
                    path.display(),
                    T::KIND,
                    self.version,
                    T::CURRENT
                ),
            )),
            VersionStatus::Newer => Some(
                Warning::new(
                    WarningCode::StateVersionNewer,
                    format!(
                        "{} was written by a newer omaddons ({} version {}); writes are refused",
                        path.display(),
                        T::KIND,
                        self.version
                    ),
                )
                .with_fix(Suggestion::text("upgrade omaddons")),
            ),
        }
    }
}

/// Loads `path`; `None` when it does not exist.
pub fn load<T: Versioned>(path: &Path) -> Result<Option<Loaded<T>>> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        // ubs:ignore — an io::ErrorKind, not a secret
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(Error::io(path, &err, status_fix())),
    };
    let (version, raw) = parse(path, &bytes)?;
    let (status, raw) = match version.cmp(&T::CURRENT) {
        Ordering::Equal => (VersionStatus::Current, raw),
        Ordering::Less => (VersionStatus::Older, T::migrate(version, raw)?),
        Ordering::Greater => (VersionStatus::Newer, raw),
    };
    let value = serde_json::from_value::<T>(raw).map_err(|err| match status {
        VersionStatus::Newer => too_new::<T>(path, version),
        _ => corrupt(path, &format!("it does not decode as {}: {err}", T::KIND)),
    })?;
    Ok(Some(Loaded {
        value,
        version,
        status,
    }))
}

/// Writes `value` at the current version through `mutate`. The file on disk is loaded
/// again first, so a newer file (even one written since the caller's load) refuses
/// with `state_too_new`, and a corrupt one with `state_corrupt` rather than being
/// replaced. A caller that loaded the file as newer is refused too.
pub fn save<T: Versioned>(
    s: &mut WriteSession<'_>,
    path: &Path,
    value: &T,
    loaded_as: Option<&VersionStatus>,
) -> Result<()> {
    if let Some(VersionStatus::Newer) = loaded_as {
        return Err(too_new::<T>(path, T::CURRENT));
    }
    if let Some(on_disk) = load::<T>(path)?
        && on_disk.status == VersionStatus::Newer
    {
        return Err(too_new::<T>(path, on_disk.version));
    }
    #[derive(Serialize)]
    struct Document<'a, T> {
        schema_version: SchemaVersion,
        #[serde(flatten)]
        value: &'a T,
    }
    let mut json = serde_json::to_vec_pretty(&Document {
        schema_version: T::CURRENT,
        value,
    })
    .map_err(|err| Error::internal(format!("serializing {}: {err}", T::KIND)))?;
    json.push(b'\n');
    let op = Op::WriteFile {
        content: Content::Bytes(&json),
        mode: Some(0o644),
    };
    mutate(s, path, op)?;
    Ok(())
}

/// The version and the rest of the document.
fn parse(path: &Path, bytes: &[u8]) -> Result<(SchemaVersion, serde_json::Value)> {
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Err(corrupt(path, "it is empty"));
    }
    let mut doc: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|err| corrupt(path, &format!("it is not valid JSON: {err}")))?;
    let object = doc
        .as_object_mut()
        .ok_or_else(|| corrupt(path, "it is not a JSON object"))?;
    let version = object
        .remove("schema_version")
        .ok_or_else(|| corrupt(path, "it has no schema_version"))?;
    let version = version
        .as_str()
        .and_then(|v| v.parse::<SchemaVersion>().ok())
        .ok_or_else(|| {
            corrupt(
                path,
                &format!("its schema_version {version} is not MAJOR.MINOR"),
            )
        })?;
    Ok((version, doc))
}

fn corrupt(path: &Path, why: &str) -> Error {
    Error::new(
        ErrorCode::StateCorrupt,
        format!("{} is corrupt: {why}", path.display()),
        status_fix(),
    )
    .details(serde_json::json!({ "path": path.display().to_string() }))
}

fn too_new<T: Versioned>(path: &Path, version: SchemaVersion) -> Error {
    Error::new(
        ErrorCode::StateTooNew,
        format!(
            "{} was written by a newer omaddons ({} version {version}; this binary writes {})",
            path.display(),
            T::KIND,
            T::CURRENT
        ),
        Suggestion::text("upgrade omaddons"),
    )
    .details(serde_json::json!({ "path": path.display().to_string(), "schema_version": version.to_string() }))
}

/// DEC-5 until the doctor lands.
fn status_fix() -> Suggestion {
    Suggestion::command(["omaddons", "status"])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsx::{ScopeTable, Writer};
    use crate::state::paths::Paths;
    use proptest::prelude::*;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    /// Version 1.1; 1.0 called `name` `title`.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
    struct TestDoc {
        name: String,
        #[serde(default)]
        count: u32,
        items: BTreeMap<String, u32>,
    }

    impl Versioned for TestDoc {
        const KIND: &'static str = "test_doc";
        const CURRENT: SchemaVersion = SchemaVersion::new(1, 1);

        fn migrate(from: SchemaVersion, mut raw: serde_json::Value) -> Result<serde_json::Value> {
            if from < SchemaVersion::new(1, 1)
                && let Some(object) = raw.as_object_mut()
                && let Some(title) = object.remove("title")
            {
                object.insert("name".to_owned(), title);
            }
            Ok(raw)
        }
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        scopes: ScopeTable,
        path: PathBuf,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let paths = Paths::with_roots(
            &root,
            root.join(".config"),
            root.join(".local/state"),
            root.join(".cache"),
        );
        std::fs::create_dir_all(paths.state_dir()).unwrap();
        let scopes = ScopeTable::new(&paths, &[], None).unwrap();
        let path = paths.state_dir().join("doc.json");
        Fixture {
            _dir: dir,
            scopes,
            path,
        }
    }

    fn doc() -> TestDoc {
        TestDoc {
            name: "retail".into(),
            count: 2,
            items: BTreeMap::from([("b".into(), 2), ("a".into(), 1)]),
        }
    }

    fn save_doc(f: &Fixture, value: &TestDoc, loaded_as: Option<&VersionStatus>) -> Result<()> {
        let mut s = WriteSession::untracked(Writer::Mutating, &f.scopes, "test");
        save(&mut s, &f.path, value, loaded_as)
    }

    #[test]
    fn versions_parse_compare_and_display() {
        let v = |s: &str| s.parse::<SchemaVersion>().unwrap();
        assert!(v("1.10") > v("1.9"));
        assert!(v("2.0") > v("1.99"));
        assert_eq!(v("1.10").to_string(), "1.10");
        for bad in ["1", "1.", ".1", "1.2.3", "a.b", "-1.0", "", "1.+2"] {
            assert!(bad.parse::<SchemaVersion>().is_err(), "{bad:?}");
        }
        assert_eq!(serde_json::to_value(v("1.0")).unwrap(), "1.0");
    }

    #[test]
    fn save_then_load_round_trips_with_the_version_first() {
        let f = fixture();
        save_doc(&f, &doc(), None).unwrap();
        let text = std::fs::read_to_string(&f.path).unwrap();
        assert!(
            text.starts_with("{\n  \"schema_version\": \"1.1\",\n  \"name\": \"retail\""),
            "{text}"
        );
        assert!(text.ends_with("}\n"));
        assert!(
            text.find("\"a\"").unwrap() < text.find("\"b\"").unwrap(),
            "maps are sorted"
        );
        let loaded = load::<TestDoc>(&f.path).unwrap().unwrap();
        assert_eq!(
            loaded,
            Loaded {
                value: doc(),
                version: TestDoc::CURRENT,
                status: VersionStatus::Current
            }
        );
        assert_eq!(loaded.warning(&f.path), None);
        assert_eq!(
            load::<TestDoc>(&f.path.with_file_name("absent.json")).unwrap(),
            None
        );
    }

    #[test]
    fn older_files_migrate_and_the_next_save_upgrades_them() {
        let f = fixture();
        std::fs::write(
            &f.path,
            r#"{"schema_version":"1.0","title":"era","items":{}}"#,
        )
        .unwrap();
        let loaded = load::<TestDoc>(&f.path).unwrap().unwrap();
        assert_eq!(
            (loaded.status, loaded.version),
            (VersionStatus::Older, SchemaVersion::new(1, 0))
        );
        assert_eq!(loaded.value.name, "era");
        assert_eq!(loaded.value.count, 0, "added fields take their default");
        assert_eq!(
            loaded.warning(&f.path).unwrap().code,
            WarningCode::StateVersionOlder
        );
        save_doc(&f, &loaded.value, Some(&loaded.status)).unwrap();
        let reloaded = load::<TestDoc>(&f.path).unwrap().unwrap();
        assert_eq!(
            (reloaded.status, reloaded.value.name.as_str()),
            (VersionStatus::Current, "era")
        );
    }

    #[test]
    fn newer_files_are_read_but_never_written() {
        let f = fixture();
        let newer = r#"{"schema_version":"2.0","name":"x","items":{},"added_in_2":true}"#;
        std::fs::write(&f.path, newer).unwrap();
        let loaded = load::<TestDoc>(&f.path).unwrap().unwrap();
        assert_eq!(loaded.status, VersionStatus::Newer);
        assert_eq!(
            loaded.warning(&f.path).unwrap().code,
            WarningCode::StateVersionNewer
        );
        for loaded_as in [
            Some(&VersionStatus::Newer),
            None,
            Some(&VersionStatus::Current),
        ] {
            let err = save_doc(&f, &doc(), loaded_as).unwrap_err();
            assert_eq!((err.code, err.exit().code()), (ErrorCode::StateTooNew, 4));
            assert_eq!(
                std::fs::read_to_string(&f.path).unwrap(),
                newer,
                "byte-identical"
            );
        }
        std::fs::write(&f.path, r#"{"schema_version":"3.0","reshaped":[]}"#).unwrap();
        let err = load::<TestDoc>(&f.path).unwrap_err();
        assert_eq!(
            err.code,
            ErrorCode::StateTooNew,
            "an undecodable newer file"
        );
    }

    #[test]
    fn a_newer_write_between_load_and_save_is_not_clobbered() {
        let f = fixture();
        save_doc(&f, &doc(), None).unwrap();
        let loaded = load::<TestDoc>(&f.path).unwrap().unwrap();
        std::fs::write(&f.path, r#"{"schema_version":"9.0","name":"newer"}"#).unwrap();
        let err = save_doc(&f, &loaded.value, Some(&loaded.status)).unwrap_err();
        assert_eq!(err.code, ErrorCode::StateTooNew);
        assert!(std::fs::read_to_string(&f.path).unwrap().contains("newer"));
    }

    #[test]
    fn corrupt_files_are_errors_never_defaults() {
        let f = fixture();
        for content in [
            "",
            "  \n",
            "{\"schema_version\":\"1.1\",\"name\":",
            "[1,2]",
            r#"{"name":"x","items":{}}"#,
            r#"{"schema_version":"one","name":"x"}"#,
            r#"{"schema_version":"1.1","name":3}"#,
        ] {
            std::fs::write(&f.path, content).unwrap();
            let err = load::<TestDoc>(&f.path).unwrap_err();
            assert_eq!(
                (err.code, err.exit().code()),
                (ErrorCode::StateCorrupt, 1),
                "{content:?}"
            );
            assert!(err.message.contains("doc.json"), "{}", err.message);
            assert_eq!(err.details["path"], f.path.display().to_string());
            let err = save_doc(&f, &doc(), None).unwrap_err();
            assert_eq!(
                err.code,
                ErrorCode::StateCorrupt,
                "saving over a corrupt file: {content:?}"
            );
        }
    }

    proptest! {
        #[test]
        fn arbitrary_bytes_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..200)) {
            let f = fixture();
            std::fs::write(&f.path, &bytes).unwrap();
            let _ = load::<TestDoc>(&f.path);
        }
    }
}
