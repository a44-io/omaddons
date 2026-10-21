//! `<cache>/omaddons/doorbell.json`: the bar widget's one signal that what
//! `status --cached` reports may have changed (M3-D4). Every write that can change that
//! output rings it once, after the write: an engine commit window (undo's too), a
//! recovery that rolled a journal back or forward, a release memo write that changed the
//! memo, and every settings command. Reads never ring, so `status` stays write-free.
//!
//! The widget watches the cache directory, since every fsx write is a temp file plus a
//! rename, and then reads `status --cached` again: the body is for debugging and
//! attribution, never a source of state. Ringing is best effort. A doorbell that cannot
//! be written never fails a command, and deleting the file is safe: the next ring writes
//! it again.

use jiff::Timestamp;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::paths::Paths;
use super::store::{self, SchemaVersion, Versioned};
use crate::error::Result;
use crate::fsx::{Content, Op, ScopeTable, WriteSession, Writer, ensure_dir_all, mutate};
use crate::model::Actor;

/// The last ring.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Doorbell {
    #[schemars(with = "String")]
    pub at: Timestamp,
    /// The command that rang, as its envelope's `command` names it.
    pub command: String,
    pub actor: Actor,
}

impl Versioned for Doorbell {
    const KIND: &'static str = "doorbell";
    const CURRENT: SchemaVersion = SchemaVersion::new(1, 0);
}

/// Rings the doorbell for `command`, run by `actor`, at [`crate::clock::now`]. A failure
/// is ignored: the widget's fallback poll covers a ring that was lost.
pub fn ring(paths: &Paths, command: &str, actor: Actor) {
    let bell = Doorbell {
        at: crate::clock::now(),
        command: command.to_owned(),
        actor,
    };
    let _ = write(paths, &bell);
}

fn write(paths: &Paths, bell: &Doorbell) -> Result<()> {
    let json = store::encode(bell)?;
    let scopes = ScopeTable::new(paths, &[], None)?;
    let mut s = WriteSession::untracked(Writer::Derived, &scopes, "doorbell");
    ensure_dir_all(&mut s, paths.cache_dir())?;
    let op = Op::WriteFile {
        content: Content::Bytes(&json),
        mode: Some(0o644),
    };
    mutate(&mut s, &paths.doorbell_file(), op)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(root: &std::path::Path) -> Paths {
        Paths::with_roots(
            root,
            root.join(".config"),
            root.join(".local/state"),
            root.join(".cache"),
        )
    }

    fn rung(paths: &Paths) -> store::Loaded<Doorbell> {
        store::load::<Doorbell>(&paths.doorbell_file())
            .unwrap()
            .expect("the doorbell rang")
    }

    #[test]
    fn a_ring_writes_a_versioned_doorbell_and_rewrites_it_each_time() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let paths = paths(&root);
        ring(&paths, "install", Actor::Widget);
        let first = rung(&paths);
        assert_eq!(first.version, SchemaVersion::new(1, 0));
        assert_eq!(first.status, store::VersionStatus::Current);
        assert_eq!(
            (first.value.command.as_str(), first.value.actor),
            ("install", Actor::Widget)
        );
        let text = std::fs::read_to_string(paths.doorbell_file()).unwrap();
        let at = |key: &str| text.find(&format!("\"{key}\":")).unwrap();
        assert!(
            at("schema_version") < at("at")
                && at("at") < at("command")
                && at("command") < at("actor"),
            "{text}"
        );
        assert!(text.contains(r#""actor": "widget""#), "{text}");

        ring(&paths, "config set", Actor::Human);
        assert_eq!(rung(&paths).value.command, "config set");

        std::fs::remove_file(paths.doorbell_file()).unwrap();
        ring(&paths, "undo", Actor::Agent);
        assert_eq!(
            rung(&paths).value.command,
            "undo",
            "a deleted doorbell comes back"
        );
    }

    #[test]
    fn a_doorbell_that_cannot_be_written_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let paths = paths(&root);
        // A file where the cache directory belongs: nothing can be written under it.
        std::fs::write(root.join(".cache"), b"not a directory").unwrap();
        ring(&paths, "install", Actor::Human);
        assert!(store::load::<Doorbell>(&paths.doorbell_file()).is_err());
        assert_eq!(
            std::fs::read(root.join(".cache")).unwrap(),
            b"not a directory"
        );
    }
}
