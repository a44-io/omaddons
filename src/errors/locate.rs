//! Where a client keeps BugGrabber's database (M3-D44): `!BugGrabber.lua` in each
//! account's `SavedVariables/`, and the file of every installed addon whose loaded TOC
//! lists `BugGrabberDB` under `## SavedVariables` (packs that embed BugGrabber, as bdUI,
//! Decursive and AdiBags do). Account-wide only: `BugGrabberDB` is an account-wide
//! global.
//!
//! The walk is bounded ([`ERRORS_MAX_ACCOUNTS`] account folders, [`ERRORS_MAX_FILES`]
//! files), and a bound hit is [`Coverage::Truncated`], never a clean result. A
//! `SavedVariables/` folder undo leaves alone (outside the client, or spelled two ways)
//! is skipped here too and counted. Account folder names are personal data: accounts are
//! numbered in the order of their names, and no type here prints or serializes a name.

use std::fmt;
use std::path::PathBuf;

use sha2::{Digest, Sha256};

use crate::discover::find_children;
use crate::engine::swap::{SvSkip, sv_skip};
use crate::fsx::FileStamp;
use crate::inventory::{FolderClass, Inventory};
use crate::limits::{ERRORS_MAX_ACCOUNTS, ERRORS_MAX_FILES};
use crate::state::paths::ClientPaths;

/// The global BugGrabber keeps its database in.
pub const GLOBAL: &str = "BugGrabberDB";
/// BugGrabber's folder, whose SavedVariables file is `!BugGrabber.lua`.
pub const FOLDER: &str = "!BugGrabber";

/// One file that may hold a `BugGrabberDB`.
#[derive(Clone, PartialEq, Eq)]
pub struct DbFile {
    /// The account's 1-based ordinal in the sorted account folder names (never the
    /// name).
    pub account: usize,
    /// `!BugGrabber`, or the folder of the addon that embeds the database.
    pub folder: String,
    /// Where it is: for reading only, never shown or serialized.
    pub path: PathBuf,
    pub stamp: FileStamp,
}

impl fmt::Debug for DbFile {
    /// Without the path, which names the account.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DbFile")
            .field("account", &self.account)
            .field("folder", &self.folder)
            .field("size", &self.stamp.size())
            .finish_non_exhaustive()
    }
}

/// Whether the client captures Lua errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capture {
    /// BugGrabber is installed, or left a database behind.
    BugGrabber,
    None,
}

/// `SavedVariables/` folders left alone, by why (as undo does).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Skipped {
    /// It resolves outside the client folder.
    pub symlinked: usize,
    /// A folder on its way is spelled two ways.
    pub two_spellings: usize,
}

/// How much of the client the walk saw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Coverage {
    Complete,
    /// A bound was hit: what was examined.
    Truncated {
        accounts: usize,
        files: usize,
    },
}

/// One `SavedVariables/` folder examined: a fingerprint of its file names, which changes
/// when a file comes or goes, not when one is rewritten.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listing {
    pub account: usize,
    /// `sha256:` and the hex SHA-256 of the sorted file names, one per line.
    pub fingerprint: String,
}

/// What [`locate`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Located {
    pub files: Vec<DbFile>,
    pub capture: Capture,
    pub skipped: Skipped,
    pub coverage: Coverage,
    pub listings: Vec<Listing>,
}

/// An installed folder whose loaded TOC lists `BugGrabberDB` (or BugGrabber's own).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declared {
    pub folder: String,
}

/// The files of `client` that may hold BugGrabber's database. `inventory` must hold the
/// loaded TOCs (`Depth::Full`): an addon embeds the database when its TOC declares it.
pub fn locate(client: &ClientPaths, inventory: &Inventory) -> Located {
    let installed = inventory
        .folders
        .iter()
        .any(|f| f.name.eq_ignore_ascii_case(FOLDER));
    let declared: Vec<Declared> = inventory
        .folders
        .iter()
        .filter(|f| f.class != FolderClass::Invalid)
        .filter(|f| {
            f.toc.as_ref().is_some_and(|toc| {
                toc.saved_variables
                    .iter()
                    .any(|v| v.eq_ignore_ascii_case(GLOBAL))
            })
        })
        .map(|f| Declared {
            folder: f.name.clone(),
        })
        .collect();
    locate_in(client, installed, &declared)
}

/// [`locate`], from whether BugGrabber is installed and the folders that declare its
/// database.
pub fn locate_in(client: &ClientPaths, installed: bool, declared: &[Declared]) -> Located {
    // The file stems to look for: BugGrabber's own, then each embedding folder's.
    let mut stems: Vec<&str> = vec![FOLDER];
    for d in declared {
        if !stems.iter().any(|s| s.eq_ignore_ascii_case(&d.folder)) {
            stems.push(&d.folder);
        }
    }
    let mut accounts: Vec<(String, PathBuf)> = Vec::new();
    for wtf in find_children(client.client_dir(), "WTF") {
        for root in find_children(&wtf, "Account") {
            for (name, dir) in dirs(&root) {
                accounts.push((name, dir));
            }
        }
    }
    accounts.sort();
    let mut located = Located {
        files: Vec::new(),
        capture: Capture::None,
        skipped: Skipped::default(),
        coverage: Coverage::Complete,
        listings: Vec::new(),
    };
    let mut files_cut = false;
    let examined = accounts.len().min(ERRORS_MAX_ACCOUNTS);
    for (at, (_, account_dir)) in accounts.iter().take(examined).enumerate() {
        let account = at + 1;
        for sv in find_children(account_dir, "SavedVariables") {
            match sv_skip(client, &sv) {
                Some(SvSkip::Symlinked) => {
                    located.skipped.symlinked += 1;
                    continue;
                }
                Some(SvSkip::TwoSpellings) => {
                    located.skipped.two_spellings += 1;
                    continue;
                }
                None => {}
            }
            let names = file_names(&sv);
            located.listings.push(Listing {
                account,
                fingerprint: fingerprint(&names),
            });
            for name in &names {
                let Some(stem) = name
                    .len()
                    .checked_sub(4)
                    .filter(|&cut| name.is_char_boundary(cut))
                    .filter(|&cut| name[cut..].eq_ignore_ascii_case(".lua"))
                    .map(|cut| &name[..cut])
                else {
                    continue;
                };
                let Some(folder) = stems.iter().find(|s| s.eq_ignore_ascii_case(stem)) else {
                    continue;
                };
                if located.files.len() == ERRORS_MAX_FILES {
                    files_cut = true;
                    break;
                }
                let path = sv.join(name);
                if let Ok(Some(stamp)) = FileStamp::read(&path)
                    && stamp.is_file()
                {
                    located.files.push(DbFile {
                        account,
                        folder: (*folder).to_owned(),
                        path,
                        stamp,
                    });
                }
            }
        }
    }
    if examined < accounts.len() || files_cut {
        located.coverage = Coverage::Truncated {
            accounts: examined,
            files: located.files.len(),
        };
    }
    if installed || !located.files.is_empty() {
        located.capture = Capture::BugGrabber;
    }
    located
}

/// The folders in `dir` (symlinks to folders included), by name; nothing when it cannot
/// be read.
fn dirs(dir: &std::path::Path) -> Vec<(String, PathBuf)> {
    let Ok(listing) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    listing
        .filter_map(Result::ok)
        .filter(|e| e.path().is_dir())
        .filter_map(|e| Some((e.file_name().into_string().ok()?, e.path())))
        .collect()
}

/// The names in `dir` that are not folders, sorted.
fn file_names(dir: &std::path::Path) -> Vec<String> {
    let Ok(listing) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = listing
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| !t.is_dir()))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn fingerprint(names: &[String]) -> String {
    let mut hasher = Sha256::new();
    for name in names {
        hasher.update(name.as_bytes());
        hasher.update(b"\n");
    }
    let hex: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("sha256:{hex}")
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    /// A client with `accounts` account folders, each with an account-wide
    /// `SavedVariables/` holding `files`.
    fn make_client(accounts: &[&str], files: &[&str]) -> (tempfile::TempDir, ClientPaths) {
        let dir = tempfile::tempdir().unwrap();
        for account in accounts {
            let sv = dir
                .path()
                .join("WTF/Account")
                .join(account)
                .join("SavedVariables");
            fs::create_dir_all(&sv).unwrap();
            for file in files {
                fs::write(sv.join(file), b"\r\nBugGrabberDB = {\r\n}\r\n").unwrap();
            }
        }
        let paths = ClientPaths::new(fs::canonicalize(dir.path()).unwrap());
        (dir, paths)
    }

    fn declared(folders: &[&str]) -> Vec<Declared> {
        folders
            .iter()
            .map(|f| Declared {
                folder: (*f).to_owned(),
            })
            .collect()
    }

    #[test]
    fn bug_grabber_files_are_found_per_account_and_embedded_ones_by_their_toc() {
        let (_dir, client) = make_client(
            &["FAKEACCT", "OTHERACCT"],
            &["!BugGrabber.lua", "bdUI.lua", "Details.lua"],
        );
        let found = locate_in(&client, true, &declared(&["bdUI"]));
        let files: Vec<(usize, &str)> = found
            .files
            .iter()
            .map(|f| (f.account, f.folder.as_str()))
            .collect();
        assert_eq!(
            files,
            [
                (1, "!BugGrabber"),
                (1, "bdUI"),
                (2, "!BugGrabber"),
                (2, "bdUI")
            ]
        );
        assert_eq!(
            (found.capture, found.coverage),
            (Capture::BugGrabber, Coverage::Complete)
        );
        assert_eq!(found.listings.len(), 2);
        // A database BugGrabber left behind is read without the folder.
        let left = locate_in(&client, false, &[]);
        assert_eq!((left.files.len(), left.capture), (2, Capture::BugGrabber));
    }

    #[test]
    fn a_client_without_bug_grabber_captures_nothing() {
        let (_dir, client) = make_client(&["FAKEACCT"], &["Details.lua"]);
        let found = locate_in(&client, false, &[]);
        assert!(found.files.is_empty());
        assert_eq!(found.capture, Capture::None);
        let (_none, bare) = make_client(&[], &[]);
        assert_eq!(locate_in(&bare, false, &[]).capture, Capture::None);
        assert_eq!(locate_in(&bare, true, &[]).capture, Capture::BugGrabber);
    }

    #[test]
    fn a_symlinked_or_two_spelled_folder_is_skipped_and_counted() {
        let (dir, client) = make_client(&["FAKEACCT"], &["!BugGrabber.lua"]);
        let outside = tempfile::tempdir().unwrap();
        let elsewhere = outside.path().join("SavedVariables");
        fs::create_dir_all(&elsewhere).unwrap();
        fs::write(elsewhere.join("!BugGrabber.lua"), b"x").unwrap();
        let linked = dir.path().join("WTF/Account/LINKED");
        fs::create_dir_all(&linked).unwrap();
        std::os::unix::fs::symlink(&elsewhere, linked.join("SavedVariables")).unwrap();
        let twice = dir.path().join("WTF/Account/TWICE");
        fs::create_dir_all(twice.join("SavedVariables")).unwrap();
        fs::create_dir_all(twice.join("savedvariables")).unwrap();
        let found = locate_in(&client, true, &[]);
        assert_eq!(
            found.skipped,
            Skipped {
                symlinked: 1,
                two_spellings: 2
            }
        );
        let accounts: Vec<usize> = found.files.iter().map(|f| f.account).collect();
        assert_eq!(
            accounts,
            [1],
            "FAKEACCT sorts first; the others are skipped"
        );
    }

    #[test]
    fn the_walk_stops_at_its_bounds_and_says_so() {
        let names: Vec<String> = (0..40).map(|i| format!("ACCOUNT{i:02}")).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let (_dir, client) = make_client(&refs, &["!BugGrabber.lua"]);
        let found = locate_in(&client, true, &[]);
        assert_eq!(
            found.coverage,
            Coverage::Truncated {
                accounts: ERRORS_MAX_ACCOUNTS,
                files: ERRORS_MAX_ACCOUNTS
            }
        );
        assert_eq!(found.files.len(), ERRORS_MAX_ACCOUNTS);

        let stems: Vec<String> = (0..70).map(|i| format!("Pack{i:02}")).collect();
        let files: Vec<String> = stems.iter().map(|s| format!("{s}.lua")).collect();
        let file_refs: Vec<&str> = files.iter().map(String::as_str).collect();
        let (_many, client) = make_client(&["FAKEACCT"], &file_refs);
        let stem_refs: Vec<&str> = stems.iter().map(String::as_str).collect();
        let found = locate_in(&client, true, &declared(&stem_refs));
        assert_eq!(found.files.len(), ERRORS_MAX_FILES);
        assert_eq!(
            found.coverage,
            Coverage::Truncated {
                accounts: 1,
                files: ERRORS_MAX_FILES
            }
        );
    }

    #[test]
    fn the_listing_fingerprint_follows_the_file_set_only() {
        let (dir, client) = make_client(&["FAKEACCT"], &["!BugGrabber.lua", "Foo.lua"]);
        let sv = dir.path().join("WTF/Account/FAKEACCT/SavedVariables");
        let print = || {
            locate_in(&client, true, &[]).listings[0]
                .fingerprint
                .clone()
        };
        let before = print();
        fs::write(sv.join("Foo.lua"), b"changed content").unwrap();
        let file = fs::File::options()
            .write(true)
            .open(sv.join("Foo.lua"))
            .unwrap();
        file.set_modified(std::time::SystemTime::UNIX_EPOCH)
            .unwrap();
        assert_eq!(print(), before, "content and times do not count");
        fs::write(sv.join("Bar.lua"), b"x").unwrap();
        let added = print();
        assert_ne!(added, before);
        fs::remove_file(sv.join("Bar.lua")).unwrap();
        assert_eq!(print(), before);
        assert!(before.starts_with("sha256:"));
    }

    #[test]
    fn no_account_name_is_printed() {
        let (_dir, client) = make_client(&["FAKEACCT"], &["!BugGrabber.lua"]);
        let found = locate_in(&client, true, &[]);
        let text = format!("{found:?}");
        assert!(!text.contains("FAKEACCT"), "{text}");
        assert!(text.contains("account: 1"), "{text}");
    }
}
