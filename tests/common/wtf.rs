//! Synthetic `WTF/` trees: accounts, realms and characters with SavedVariables, under the
//! fake names of [`super::privacy`] unless a test picks others, plus a large tree for
//! the copy budget. Deterministic: the same calls build byte-identical trees.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// A `WTF/` under a client dir, built by [`Wtf::build`].
pub struct Wtf {
    client: PathBuf,
    account: String,
    realm: String,
    character: String,
    files: Vec<(String, Vec<u8>)>,
    symlinks: Vec<(String, String)>,
    modes: Vec<(String, u32)>,
}

impl Wtf {
    /// `<client_dir>/WTF`, for `FAKEACCT` on `FakeRealm` as `FakeChar`.
    pub fn at(client_dir: impl Into<PathBuf>) -> Self {
        Self {
            client: client_dir.into(),
            account: "FAKEACCT".to_owned(),
            realm: "FakeRealm".to_owned(),
            character: "FakeChar".to_owned(),
            files: Vec::new(),
            symlinks: Vec::new(),
            modes: Vec::new(),
        }
    }

    /// The account later entries go under.
    pub fn account(mut self, name: &str) -> Self {
        name.clone_into(&mut self.account);
        self
    }

    /// The realm later character entries go under.
    pub fn realm(mut self, name: &str) -> Self {
        name.clone_into(&mut self.realm);
        self
    }

    /// The character later character entries go under.
    pub fn character(mut self, name: &str) -> Self {
        name.clone_into(&mut self.character);
        self
    }

    /// `folder`'s SavedVariables of `bytes` bytes each: account-wide and the current
    /// character's.
    pub fn saved_variables(mut self, folder: &str, bytes: usize) -> Self {
        let body = |scope: &str| {
            let line = format!("{folder}DB = {{ [\"{scope}\"] = true }}\n");
            line.repeat(bytes / line.len() + 1).into_bytes()[..bytes].to_vec()
        };
        let account = format!("Account/{}/SavedVariables/{folder}.lua", self.account);
        let character = format!(
            "Account/{}/{}/{}/SavedVariables/{folder}.lua",
            self.account, self.realm, self.character
        );
        self.files.push((account, body("account")));
        self.files.push((character, body("character")));
        self
    }

    /// `WTF/Config.wtf`.
    pub fn config_wtf(self, text: &str) -> Self {
        self.file("Config.wtf", text.as_bytes())
    }

    /// Any file, by its path relative to `WTF/`.
    pub fn file(mut self, rel: &str, bytes: &[u8]) -> Self {
        self.files.push((rel.to_owned(), bytes.to_vec()));
        self
    }

    /// A symlink at `rel` (relative to `WTF/`) pointing at `target`, never followed.
    pub fn symlink(mut self, rel: &str, target: &str) -> Self {
        self.symlinks.push((rel.to_owned(), target.to_owned()));
        self
    }

    /// `rel`'s mode, applied once everything is in (so a `0500` directory still gets
    /// its children).
    pub fn mode(mut self, rel: &str, mode: u32) -> Self {
        self.modes.push((rel.to_owned(), mode));
        self
    }

    /// `files` SavedVariables files of about `total_bytes` in all, spread over 10
    /// characters of the current account: the copy budget's tree.
    pub fn big(mut self, total_bytes: u64, files: u64) -> Self {
        let each = usize::try_from(total_bytes / files.max(1)).unwrap();
        let block: Vec<u8> = (0..each).map(|i| b"0123456789abcdef"[i % 16]).collect();
        for n in 0..files {
            let rel = format!(
                "Account/{}/{}/Char{}/SavedVariables/Addon{n}.lua",
                self.account,
                self.realm,
                n % 10
            );
            self.files.push((rel, block.clone()));
        }
        self
    }

    /// Writes the tree; returns `<client_dir>/WTF`.
    pub fn build(self) -> PathBuf {
        let wtf = self.client.join("WTF");
        fs::create_dir_all(&wtf).unwrap();
        for (rel, bytes) in &self.files {
            let path = wtf.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, bytes).unwrap();
        }
        for (rel, target) in &self.symlinks {
            let path = wtf.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::os::unix::fs::symlink(target, &path).unwrap();
        }
        for (rel, mode) in &self.modes {
            let path = wtf.join(rel);
            if !path.exists() {
                fs::create_dir_all(&path).unwrap();
            }
            fs::set_permissions(&path, fs::Permissions::from_mode(*mode)).unwrap();
        }
        wtf
    }
}

/// Gives every directory below `dir` (and `dir`) owner access again, so a tempdir
/// holding a `0500` or `0000` one can be deleted.
pub fn writable_again(dir: &Path) {
    let Ok(meta) = fs::symlink_metadata(dir) else {
        return;
    };
    if !meta.is_dir() {
        return;
    }
    let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o755));
    if let Ok(listing) = fs::read_dir(dir) {
        for entry in listing.flatten() {
            writable_again(&entry.path());
        }
    }
}
