//! The synthetic home of the run-the-fix corpus (M4-D15): retail and Classic Era
//! clients from Battle.net and a 3.3.5a custom client, DBM managed on both Battle.net
//! clients one release behind, Questie on Classic Era, and the fixtures the corpus fixes
//! reach. Each run needs an untouched copy, and state holds absolute paths, so a copy at
//! another directory would still reach the original's clients: the home stays at one
//! directory and is restored there, byte for byte, from a snapshot.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use super::home::Home;
use super::http::{Asset, gh_release, github_search, paths};
use super::tlog::{Cmd, TestLog};
use super::trees::{CustomRoot, EntryKind, copy_tree_exact, tree_entries};
use super::tukui::{elvui, tukui_list};
use super::wowi::{FileEntry, wowi_filelist};
use super::wtf::{Wtf, writable_again};

pub const DBM_BASE: &str = "https://github.com/DeadlyBossMods/DeadlyBossMods";
pub const QUESTIE_BASE: &str = "https://github.com/Questie/Questie";
pub const OR_BASE: &str = "https://github.com/o/r";
pub const DBM_INSTALLED: &str = "12.1.14";
pub const DBM_LATEST: &str = "12.1.20";
pub const QUESTIE_TAG: &str = "v11.0.0";
/// Files a command may write even when it fails; never part of `records()`.
pub const VOLATILE_STATE: &[&str] = &["net.log", "net.log.1"];

/// The synthetic home of the run-the-fix corpus (M4-D15), kept at one directory for
/// its whole life and restored there byte for byte before every run that needs an
/// untouched home: state holds absolute paths, so a copy elsewhere is not independent.
pub struct IntentHome {
    /// `<slot>/` holds `env/` (the home), `pristine/` and `row/` (its snapshots).
    slot: tempfile::TempDir,
    pub home: Home,
    pristine: PathBuf,
    row: PathBuf,
}

/// DBM's release `version`: DBM-Core and DBM-GUI, loading on retail and Classic Era.
fn dbm_entries(version: &str) -> Vec<(String, String)> {
    let toc = |extra: &str| format!("## Interface: 11509, 120105\n{extra}");
    vec![
        (
            "DBM-Core/DBM-Core.toc".to_owned(),
            toc(&format!("## Title: DBM\n## Version: {version}\n")),
        ),
        (
            "DBM-Core/DBM-Core.lua".to_owned(),
            "-- DBM-Core\n".to_owned(),
        ),
        (
            "DBM-GUI/DBM-GUI.toc".to_owned(),
            toc("## Dependencies: DBM-Core\n"),
        ),
    ]
}

fn borrowed(entries: &[(String, String)]) -> Vec<(&str, &str)> {
    entries
        .iter()
        .map(|(path, text)| (path.as_str(), text.as_str()))
        .collect()
}

/// Serves DBM `version` as its latest release.
fn serve_dbm(home: &Home, version: &str) {
    let entries = dbm_entries(version);
    let file = format!("DBM-{version}.zip");
    let flavors: &[(&str, u32)] = &[("mainline", 120_105), ("classic", 11_509)];
    let assets: [Asset<'_>; 1] = [(file.as_str(), flavors, borrowed(&entries))];
    gh_release(&home.http, DBM_BASE, version, &assets);
}

/// Builds the corpus home; see [`IntentHome`].
pub fn intent_home() -> IntentHome {
    let slot = tempfile::tempdir().unwrap();
    let mut home = Home::at(&slot.path().join("env"), &["wow", "wow_classic_era"]);

    // SavedVariables under the fake account, realm and character names.
    Wtf::at(home.client("_retail_"))
        .saved_variables("DBM-Core", 512)
        .build();
    Wtf::at(home.client("_classic_era_"))
        .saved_variables("DBM-Core", 512)
        .saved_variables("Questie", 512)
        .build();

    serve_dbm(&home, DBM_INSTALLED);
    let questie: [Asset<'_>; 1] = [(
        "Questie-v11.0.0.zip",
        &[("classic", 11_509)],
        vec![
            (
                "Questie/Questie.toc",
                "## Interface: 11509\n## Title: Questie\n",
            ),
            ("QuestieDB/QuestieDB.toc", "## Interface: 11509\n"),
        ],
    )];
    gh_release(&home.http, QUESTIE_BASE, QUESTIE_TAG, &questie);
    // Catalog names, so the records carry `DBM` and `Questie`.
    home.run(&["install", "dbm", "-t", "retail"]).expect(0);
    home.run(&["install", "dbm", "-t", "era"]).expect(0);
    home.run(&["install", "questie", "-t", "era"]).expect(0);

    home.with_custom(CustomRoot::at(home.env.root().join("pserver/wotlk")));
    let targets = home.run(&["targets"]).expect(0);
    let ids: Vec<&str> = targets["data"]["targets"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["id"].as_str())
        .collect();
    assert!(
        ids.contains(&"wotlk"),
        "the custom 3.3.5a root did not register as wotlk: {ids:?}"
    );
    home.run(&["targets", "default", "retail"]).expect(0);

    // DBM is now one release behind, and the memo old enough that a check asks again.
    serve_dbm(&home, DBM_LATEST);
    let (mut memo, _) = omaddons::state::memo::load(&paths(&home.env));
    for entry in memo.entries.values_mut() {
        entry.fetched_at -= jiff::SignedDuration::from_hours(24);
    }
    home.seed_memo(&memo);
    let r: [Asset<'_>; 1] = [(
        "r-v1.zip",
        &[("mainline", 120_105)],
        vec![("R/R.toc", "## Interface: 120105\n## Title: R\n")],
    )];
    gh_release(&home.http, OR_BASE, "v1", &r);
    wowi_filelist(
        &home.http,
        &[FileEntry::new(8814, "Deadly Boss Mods", &["DBM-Core"])],
    );
    tukui_list(&home.http, &[elvui()]);
    github_search(&home.http, "unknownthing", &[]);

    // A catalog name that is also a game mode, for the collision rows.
    let catalog_d = home.env.config().join("omaddons/catalog.d");
    fs::create_dir_all(&catalog_d).unwrap();
    fs::write(
        catalog_d.join("50-intent.toml"),
        "schema_version = \"1.0\"\n\n[[addon]]\nname = \"Hardcore\"\nref = \"gh:o/hardcore\"\nfolders = [\"Hardcore\"]\n",
    )
    .unwrap();

    let intent = IntentHome {
        pristine: slot.path().join("pristine"),
        row: slot.path().join("row"),
        slot,
        home,
    };
    fs::create_dir_all(intent.work_dir()).unwrap();
    copy_tree_exact(intent.env_root(), &intent.pristine);
    intent
}

impl IntentHome {
    /// The client ids, in id order.
    pub const CLIENTS: [&'static str; 3] = ["era", "retail", "wotlk"];

    pub fn env_root(&self) -> &Path {
        self.home.env.root()
    }

    /// Where every command runs: relative arguments resolve inside the home.
    pub fn work_dir(&self) -> PathBuf {
        self.env_root().join("work")
    }

    /// The client directory of `id`, one of [`Self::CLIENTS`].
    pub fn client_dir(&self, id: &str) -> PathBuf {
        let dir = match id {
            "retail" => Some(self.home.client("_retail_")),
            "era" => Some(self.home.client("_classic_era_")),
            "wotlk" => Some(self.env_root().join("pserver/wotlk")),
            _ => None,
        };
        assert!(dir.is_some(), "the corpus home has no client {id}");
        dir.unwrap_or_default()
    }

    /// Puts the home back as [`intent_home`] built it.
    pub fn reset(&self) {
        self.restore(&self.pristine);
    }

    /// Snapshots the home as it is now, for [`Self::reset_row`].
    pub fn save_row(&self) {
        if fs::symlink_metadata(&self.row).is_ok() {
            writable_again(&self.row);
            fs::remove_dir_all(&self.row).unwrap();
        }
        copy_tree_exact(self.env_root(), &self.row);
    }

    /// Puts the home back as [`Self::save_row`] saw it.
    pub fn reset_row(&self) {
        self.restore(&self.row);
    }

    fn restore(&self, snapshot: &Path) {
        let env = self.env_root();
        writable_again(env);
        fs::remove_dir_all(env).unwrap();
        copy_tree_exact(snapshot, env);
    }

    /// The records: every entry under `<state>/omaddons` but the [`VOLATILE_STATE`]
    /// files at its top level (keys `state/…`), and under `<config>/omaddons` (keys
    /// `config/…`), each its kind and mode with its SHA-256 or link target. `<cache>` is
    /// derived and never a record.
    pub fn records(&self) -> BTreeMap<String, String> {
        let mut records = BTreeMap::new();
        let roots = [
            ("state", self.home.state()),
            ("config", self.home.env.config().join("omaddons")),
        ];
        for (prefix, root) in roots {
            if !root.is_dir() {
                continue;
            }
            for (path, entry) in tree_entries(&root) {
                if prefix == "state" && VOLATILE_STATE.contains(&path.as_str()) {
                    continue;
                }
                let content = match entry.kind {
                    EntryKind::File => entry.sha256.unwrap_or_default(),
                    EntryKind::Symlink => entry.target.unwrap_or_default(),
                    EntryKind::Dir => String::new(),
                };
                let value = format!("{:?} {:o} {content}", entry.kind, entry.mode);
                records.insert(format!("{prefix}/{path}"), value);
            }
        }
        records
    }

    /// `log.cmd(&self.home, argv)` in [`Self::work_dir`].
    pub fn cmd<'l>(&self, log: &'l TestLog, argv: &[&str]) -> Cmd<'l> {
        log.cmd(&self.home, argv).cwd(&self.work_dir())
    }
}

impl Drop for IntentHome {
    /// A command may leave a read-only directory; the `TempDir` must still remove it.
    fn drop(&mut self) {
        writable_again(self.slot.path());
    }
}
