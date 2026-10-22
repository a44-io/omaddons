//! `<state>/errors_ack.json`: the user's dismissals of Lua errors (M3-D19). A dismissal
//! is the user's decision, so it lives in the state directory, never the cache:
//! deleting the cache must not bring back errors the user dismissed. Per target key and
//! database it holds only the session acknowledged and when; a database is keyed by a
//! hash of its path, which names an account, so the path is never stored.

use std::collections::BTreeMap;

use jiff::Timestamp;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{Error, ErrorCode, Result, Suggestion};
use crate::fsx::WriteSession;
use crate::state::paths::Paths;
use crate::state::store::{self, SchemaVersion, VersionStatus, Versioned};

/// The file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ErrorsAck {
    /// By target key.
    #[serde(default)]
    pub targets: BTreeMap<String, TargetAck>,
}

impl Versioned for ErrorsAck {
    const KIND: &'static str = "errors_ack";
    const CURRENT: SchemaVersion = SchemaVersion::new(1, 0);
}

/// One client's dismissals.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TargetAck {
    /// By database key ([`db_key`]).
    #[serde(default)]
    pub databases: BTreeMap<String, Ack>,
}

/// One database's dismissal: its session then, and when.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Ack {
    pub acked_session: u32,
    #[schemars(with = "String")]
    pub acked_at: Timestamp,
}

impl ErrorsAck {
    /// `target`'s dismissal of the database at `rel` (relative to the client folder).
    pub fn get(&self, target: &str, rel: &str) -> Option<&Ack> {
        self.targets.get(target)?.databases.get(&db_key(rel))
    }
}

/// A database's key: the first 16 hex digits of the SHA-256 of its path relative to
/// the client folder.
pub fn db_key(rel: &str) -> String {
    Sha256::digest(rel.as_bytes())
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The dismissals as `errors` and `status` read them: a file that does not read is no
/// dismissal (with a note why), never a failure.
pub fn read(paths: &Paths) -> (ErrorsAck, Option<String>) {
    match store::load::<ErrorsAck>(&paths.errors_ack()) {
        Ok(Some(loaded)) => (loaded.value, None),
        Ok(None) => (ErrorsAck::default(), None),
        Err(_) => (
            ErrorsAck::default(),
            Some("errors_ack.json does not read: no dismissal is applied".to_owned()),
        ),
    }
}

/// The dismissals as `errors dismiss` reads them before adding to them: a file it
/// cannot read is `state_corrupt`, never overwritten.
pub fn load_for_write(paths: &Paths) -> Result<(ErrorsAck, Option<VersionStatus>)> {
    match store::load::<ErrorsAck>(&paths.errors_ack()) {
        Ok(Some(loaded)) => Ok((loaded.value, Some(loaded.status))),
        Ok(None) => Ok((ErrorsAck::default(), None)),
        Err(err) if err.code == ErrorCode::StateCorrupt => Err(Error::new(
            ErrorCode::StateCorrupt,
            "errors_ack.json, your dismissed Lua errors, does not read",
            Suggestion::text(
                "move errors_ack.json out of the state directory, or delete it: deleting it brings dismissed errors back as new",
            ),
        )),
        Err(err) => Err(err),
    }
}

/// Writes the dismissals (a newer file is refused, M3-D45).
pub fn save(
    s: &mut WriteSession<'_>,
    paths: &Paths,
    ack: &ErrorsAck,
    loaded_as: Option<&VersionStatus>,
) -> Result<()> {
    store::save(s, &paths.errors_ack(), ack, loaded_as)
}
