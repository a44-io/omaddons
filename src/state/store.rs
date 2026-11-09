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

use schemars::{JsonSchema, Schema, SchemaGenerator};
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

    /// The schema of the document `save` writes: `schema_version`, then this type's fields.
    fn schema(g: &mut SchemaGenerator) -> Schema
    where
        Self: Sized + 'static,
    {
        g.root_schema_for::<Document<'static, Self>>()
    }

    /// Rewrites an older document (`schema_version` already removed) into the current
    /// shape. Pure, so the doctor's `state-version-older` fixer (M4) can reuse it. The
    /// default suits additive minor bumps, whose new fields have serde defaults.
    fn migrate(from: SchemaVersion, raw: serde_json::Value) -> Result<serde_json::Value> {
        let _ = from;
        Ok(raw)
    }
}

/// One saved format: its name, written version and the schema of what it writes.
#[derive(Debug, Clone, Copy)]
pub struct KindRow {
    pub kind: &'static str,
    pub current: SchemaVersion,
    pub schema: fn(&mut SchemaGenerator) -> Schema,
}

impl KindRow {
    pub const fn of<T: Versioned + 'static>() -> Self {
        Self {
            kind: T::KIND,
            current: T::CURRENT,
            schema: T::schema,
        }
    }
}

/// Every persisted format, declared once with its version and schema.
pub const KINDS: &[KindRow] = &[
    KindRow::of::<super::records::TargetRecord>(),
    KindRow::of::<super::files_index::FilesIndex>(),
    KindRow::of::<super::status::StatusFile>(),
    KindRow::of::<super::registry::Registry>(),
    KindRow::of::<super::registry::TargetMarker>(),
    KindRow::of::<crate::discover::cache::DiscoveryCache>(),
    KindRow::of::<crate::net::validators::CachedMeta>(),
    KindRow::of::<super::memo::ReleaseMemo>(),
    KindRow::of::<super::about_memo::AboutMemo>(),
    KindRow::of::<crate::sources::wowi::index::StoredIndex>(),
    KindRow::of::<super::export_memo::ExportMemo>(),
    KindRow::of::<super::doorbell::Doorbell>(),
    KindRow::of::<crate::backup::BackupMeta>(),
    KindRow::of::<crate::backup::Manifest>(),
    KindRow::of::<crate::backup::export::ExportedMeta>(),
    KindRow::of::<crate::backup::AddonsFile>(),
    KindRow::of::<crate::backup::inventory::InventoryFile>(),
    KindRow::of::<super::history_tail::HistoryTail>(),
    KindRow::of::<crate::errors::memo::ErrorsMemo>(),
    KindRow::of::<crate::errors::ack::ErrorsAck>(),
    KindRow::of::<super::plans::StoredPlan>(),
    KindRow {
        kind: "history",
        current: super::history::CURRENT,
        schema: super::history::line_schema,
    },
    // A doctor run's `report.json`: the envelope's bytes, written through fsx and not
    // `save`; its version is the envelope's `schema_version`. 1.1: the `info` action in
    // `commands` (readers refuse only a newer major, `doctor::runs`).
    KindRow {
        kind: "doctor_report",
        current: SchemaVersion::new(1, 1),
        schema: crate::doctor::report::record_schema,
    },
];

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
        Err(err) => return Err(Error::io(path, &err, state_file_fix())),
    };
    load_bytes(path, &bytes).map(Some)
}

/// [`load`] of `bytes` already read from `path`: for a caller that keeps the bytes too
/// (the release memo compares them with what it would write).
pub fn load_bytes<T: Versioned>(path: &Path, bytes: &[u8]) -> Result<Loaded<T>> {
    if let Some(value) = current::<T>(bytes) {
        return Ok(Loaded {
            value,
            version: T::CURRENT,
            status: VersionStatus::Current,
        });
    }
    let (version, raw) = parse(path, bytes)?;
    let (status, raw) = match version.cmp(&T::CURRENT) {
        Ordering::Equal => (VersionStatus::Current, raw),
        Ordering::Less => (VersionStatus::Older, T::migrate(version, raw)?),
        Ordering::Greater => (VersionStatus::Newer, raw),
    };
    let value = serde_json::from_value::<T>(raw).map_err(|err| match status {
        VersionStatus::Newer => too_new::<T>(path, version),
        _ => corrupt(path, &format!("it does not decode as {}: {err}", T::KIND)),
    })?;
    Ok(Loaded {
        value,
        version,
        status,
    })
}

/// `state_too_new` when the file at `path` was written by a newer omaddons (M3-D45): the
/// check a change makes before it stages anything, so that it is never refused only at
/// its save, after its folders moved. A file that is absent or older passes; one that
/// does not read fails as [`load`] does.
pub fn refuse_newer<T: Versioned>(path: &Path) -> Result<()> {
    match load::<T>(path)? {
        Some(loaded) if loaded.status == VersionStatus::Newer => {
            Err(too_new::<T>(path, loaded.version))
        }
        _ => Ok(()),
    }
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
    let on_disk = load::<T>(path)?;
    if let Some(on_disk) = &on_disk
        && on_disk.status == VersionStatus::Newer
    {
        return Err(too_new::<T>(path, on_disk.version));
    }
    if let Some(VersionStatus::Newer) = loaded_as {
        // Loaded as newer, though the file has changed since: the version it was is
        // not known here, only that it was past this binary's.
        return Err(too_new::<T>(path, T::CURRENT));
    }
    let json = encode(value)?;
    let op = Op::WriteFile {
        content: Content::Bytes(&json),
        mode: Some(0o644),
    };
    mutate(s, path, op)?;
    Ok(())
}

/// The bytes `save` writes: pretty JSON with `schema_version` first and a trailing
/// newline. For derived files that may replace a corrupt predecessor (status.json).
pub fn encode<T: Versioned>(value: &T) -> Result<Vec<u8>> {
    let mut json = serde_json::to_vec_pretty(&Document {
        schema_version: T::CURRENT,
        value,
    })
    .map_err(|err| Error::internal(format!("serializing {}: {err}", T::KIND)))?;
    json.push(b'\n');
    Ok(json)
}

/// The same document supplies the saved bytes and the published schema.
#[derive(Serialize, JsonSchema)]
struct Document<'a, T> {
    schema_version: SchemaVersion,
    #[serde(flatten)]
    value: &'a T,
}

/// A document at `T::CURRENT`, decoded straight from its bytes, without the JSON value
/// [`parse`] builds first: the common case, and `status --cached` reads a dozen of these
/// on every widget refresh. `None` for anything else (another version, a document that
/// does not decode, duplicate keys), which [`load_bytes`] then reads the general way, so
/// its result and its errors are what they always were. The `schema_version` key is
/// ignored as an unknown field: no stored type has a field of that name or keeps unknown
/// keys.
fn current<T: Versioned>(bytes: &[u8]) -> Option<T> {
    #[derive(Deserialize)]
    struct Version<'a> {
        #[serde(borrow)]
        schema_version: &'a str,
    }
    let version = serde_json::from_slice::<Version<'_>>(bytes).ok()?;
    let current = version.schema_version.parse::<SchemaVersion>().ok()? == T::CURRENT;
    current
        .then(|| serde_json::from_slice::<T>(bytes).ok())
        .flatten()
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
        state_file_fix(),
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

/// The fix of a state file that cannot be read or decoded: the doctor's explanation of
/// `fm-schemas-state-corrupt`, which names the file (M4-D53; was DEC-5's `omaddons
/// status`). No `-t`: a store does not know whose file it is, and the explanation lists
/// every instance.
fn state_file_fix() -> Suggestion {
    crate::doctor::registry::explain_fix(crate::doctor::registry::ids::STATE_CORRUPT, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsx::{ScopeTable, Writer};
    use crate::state::paths::Paths;
    use proptest::prelude::*;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    /// Every `impl Versioned for T` outside tests, as `T`.
    fn versioned_impls(dir: &std::path::Path, found: &mut Vec<String>) {
        use crate::source_scan::{Literals, production_code, sources};

        for (_, text) in sources(dir) {
            let code = production_code(&text, Literals::Blank).unwrap_or_default();
            for (at, _) in code.match_indices("impl Versioned for ") {
                let rest = &code[at + "impl Versioned for ".len()..];
                let name: String = rest
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                found.push(name);
            }
        }
    }

    #[test]
    fn every_persisted_format_has_a_kinds_row() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut found = Vec::new();
        versioned_impls(&src, &mut found);
        let rows = std::fs::read_to_string(src.join("state/store.rs")).unwrap();
        let rows = rows.split("pub const KINDS").nth(1).unwrap_or_default();
        let rows = rows.split("];").next().unwrap_or_default();
        for name in &found {
            assert!(
                rows.contains(&format!("::{name}>()")),
                "{name} implements Versioned but has no KINDS row"
            );
        }
        assert_eq!(
            found.len() + 2,
            KINDS.len(),
            "every row but history's and doctor_report's"
        );
        let kinds: Vec<&str> = KINDS.iter().map(|row| row.kind).collect();
        assert!(kinds.contains(&"release_memo") && kinds.contains(&"http_validators"));
    }

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

    #[test]
    fn kind_rows_match_their_types() {
        let row = KindRow::of::<TestDoc>();
        assert_eq!(row.kind, "test_doc", "{row:?}");
        assert_eq!(row.current, SchemaVersion::new(1, 1), "{row:?}");
    }

    #[test]
    fn encode_matches_the_document_schema_shape() {
        let document = TestDoc {
            name: "sample".into(),
            count: 0,
            items: BTreeMap::new(),
        };
        let bytes = encode(&document).unwrap();
        assert!(
            bytes.starts_with(b"{\n  \"schema_version\": \"1.1\","),
            "schema_version is first: {}",
            String::from_utf8_lossy(&bytes)
        );
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let schema = TestDoc::schema(&mut crate::contract::schema::generator()).to_value();
        let keys = |v: &serde_json::Value| {
            v.as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect::<std::collections::BTreeSet<_>>()
        };
        assert_eq!(
            keys(&value),
            keys(&schema["properties"]),
            "bytes: {value}; schema: {schema}"
        );
        assert!(
            schema["required"]
                .as_array()
                .unwrap()
                .iter()
                .any(|key| key == "schema_version"),
            "{schema}"
        );
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
            assert_eq!(
                err.fix.argv(),
                Some(&STATE_FILE_FIX.map(str::to_owned)[..]),
                "{content:?}: the doctor's explanation"
            );
            let err = save_doc(&f, &doc(), None).unwrap_err();
            assert_eq!(
                err.code,
                ErrorCode::StateCorrupt,
                "saving over a corrupt file: {content:?}"
            );
        }
    }

    /// The fix of a state file that does not read or decode (M4-D53).
    const STATE_FILE_FIX: [&str; 4] = ["omaddons", "doctor", "explain", "fm-schemas-state-corrupt"];

    /// A state file that cannot be read (a directory in its place) is an `io` error with
    /// the same fix as a corrupt one: the doctor reports both under one failure mode.
    #[test]
    fn an_unreadable_state_file_points_at_the_doctor() {
        let f = fixture();
        std::fs::create_dir_all(&f.path).unwrap();
        let err = load::<TestDoc>(&f.path).unwrap_err();
        assert_eq!(err.code, ErrorCode::Io, "{err:?}");
        assert_eq!(
            err.fix.argv(),
            Some(&STATE_FILE_FIX.map(str::to_owned)[..]),
            "{err:?}"
        );
        assert_eq!(err.details["path"], f.path.display().to_string());
    }

    /// A newer state file keeps its advice: upgrade omaddons.
    #[test]
    fn a_newer_file_keeps_upgrade_omaddons() {
        let f = fixture();
        std::fs::write(&f.path, r#"{"schema_version":"9.0","name":3}"#).unwrap();
        let err = load::<TestDoc>(&f.path).unwrap_err();
        assert_eq!(err.code, ErrorCode::StateTooNew, "{err:?}");
        assert_eq!(
            (err.fix.display(), err.fix.argv()),
            ("upgrade omaddons", None),
            "{err:?}"
        );
    }

    /// The fix names a registered failure mode of the schemas subsystem.
    #[test]
    fn the_state_file_fix_names_a_registered_failure_mode() {
        let fm = STATE_FILE_FIX[3];
        let row = crate::doctor::registry::FAILURE_MODES
            .iter()
            .find(|row| row.id == fm);
        assert!(
            row.is_some_and(|row| row.subsystem == crate::doctor::finding::Subsystem::Schemas),
            "{fm}"
        );
        assert_eq!(
            state_file_fix().argv(),
            Some(&STATE_FILE_FIX.map(str::to_owned)[..])
        );
    }

    /// A current document decodes straight from its bytes to what the general path gives;
    /// any other (older, newer, duplicate keys, not a document) is left to that path.
    #[test]
    fn a_current_document_decodes_directly_and_anything_else_the_general_way() {
        let general = |bytes: &[u8]| {
            let (version, raw) = parse(Path::new("doc.json"), bytes).unwrap();
            assert_eq!(version, TestDoc::CURRENT);
            serde_json::from_value::<TestDoc>(raw).unwrap()
        };
        let saved = encode(&doc()).unwrap();
        assert_eq!(current::<TestDoc>(&saved), Some(general(&saved)));
        assert_eq!(current::<TestDoc>(&saved), Some(doc()));
        let compact = br#"{"items":{"a":1},"schema_version":"1.1","name":"x"}"#;
        assert_eq!(current::<TestDoc>(compact), Some(general(compact)));
        for other in [
            &br#"{"schema_version":"1.0","name":"x","items":{}}"#[..],
            br#"{"schema_version":"9.0","name":"x","items":{}}"#,
            br#"{"schema_version":"1.1","name":"x","name":"y","items":{}}"#,
            br#"{"schema_version":"1.1","name":3,"items":{}}"#,
            b"[1,2]",
            b"",
        ] {
            assert_eq!(
                current::<TestDoc>(other),
                None,
                "{}",
                String::from_utf8_lossy(other)
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
