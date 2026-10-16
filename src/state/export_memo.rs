//! `<cache>/omaddons/export.json`: the SHA-256 of each `addons.toml` that
//! `export --write` wrote, so a later `--write` can tell a file it wrote from one edited
//! since by hand or by another tool (M2-D43). A cache file: missing or unreadable means
//! "no record", and the only effect is that the next `--write` asks for `--overwrite`.

use std::collections::BTreeMap;
use std::path::Path;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::paths::Paths;
use super::store::{self, SchemaVersion, VersionStatus, Versioned};
use crate::error::Result;
use crate::fsx::{Content, Op, WriteSession, ensure_dir_all, mutate};

/// What `export --write` wrote.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ExportMemo {
    /// The SHA-256 (lowercase hex) of each file written, by its absolute path.
    #[serde(default)]
    pub written: BTreeMap<String, String>,
}

impl Versioned for ExportMemo {
    const KIND: &'static str = "export_memo";
    const CURRENT: SchemaVersion = SchemaVersion::new(1, 0);
}

impl ExportMemo {
    /// The memo on disk; missing, corrupt or newer reads as empty.
    pub fn load(paths: &Paths) -> Self {
        match store::load::<Self>(&paths.export_memo()) {
            Ok(Some(loaded)) if loaded.status != VersionStatus::Newer => loaded.value,
            _ => Self::default(),
        }
    }

    /// The hash recorded for `file`, if any.
    pub fn hash_of(&self, file: &Path) -> Option<&str> {
        self.written
            .get(&file.display().to_string())
            .map(String::as_str)
    }

    /// Records `sha256` for `file` and writes the memo through `s`.
    pub fn record(
        mut self,
        s: &mut WriteSession<'_>,
        paths: &Paths,
        file: &Path,
        sha256: &str,
    ) -> Result<()> {
        self.written
            .insert(file.display().to_string(), sha256.to_owned());
        let json = store::encode(&self)?;
        ensure_dir_all(s, paths.cache_dir())?;
        let op = Op::WriteFile {
            content: Content::Bytes(&json),
            mode: Some(0o644),
        };
        mutate(s, &paths.export_memo(), op)?;
        Ok(())
    }
}
