//! Details coverage rows: folders. Ownership refusals (`engine/ownership.rs::refusal`,
//! each code's `folders[]` carrying its own rank's entries and every lower rank's), the
//! same-name takeover and the within-transaction conflict (`engine/plan.rs`), adoption's
//! refusals of folders that are not adoptable, and a client with two AddOns spellings.

use std::fs;
use std::os::unix::fs::symlink;
use std::path::PathBuf;

use omaddons::error::ErrorCode;

use super::super::support::{Checked, Scene, check};
use super::Row;
use crate::common::addons::{toc, unpack};
use crate::common::archives::zip_text;
use crate::common::home::Home;
use crate::common::http::{Asset, gh_release};

pub const ROWS: &[Row] = &[
    EVERY_RANK,
    NOT_ADOPTABLE,
    TAKEOVER,
    WITHIN_TRANSACTION,
    UNLOADED_GROUP,
    TWO_ADDONS_SPELLINGS,
];

/// A TOC both Era and Anniversary load.
const SHARED_TOC: &str = "## Interface: 11509, 20506\n## Version: 1\n";

/// Era and Anniversary sharing one AddOns directory (Anniversary's is a symlink to
/// Era's), holding a folder of every kind an install can be refused over, as seen from
/// Anniversary: `Both` (Era records it), `Other` (Anniversary's addon Other owns it),
/// `Linked` (a symlink to a working copy), `Dup` and `dup` (two spellings of one
/// folder), `respell` (unmanaged, shipped as `Respell`), `Loose` (unmanaged) and `Inner`
/// (unmanaged, a symlink inside).
struct Shared {
    home: Home,
    dl: PathBuf,
    both: PathBuf,
}

impl Shared {
    fn new() -> Self {
        let home = Home::new(&["wow_classic_era", "wow_anniversary"]);
        let root = home.env.root().to_path_buf();
        let (era, anniversary) = (home.addons("_classic_era_"), home.addons("_anniversary_"));
        fs::create_dir_all(&era).unwrap();
        fs::create_dir_all(&anniversary).unwrap();
        let dl = root.join("dl");
        fs::create_dir_all(&dl).unwrap();
        let both = root.join("src/Both");
        unpack(&root.join("src"), &[("Both/Both.toc", SHARED_TOC)]);
        let s = Self { home, dl, both };

        // Other is recorded while the two directories are apart (a write through the
        // shared one is outside Anniversary's scope), then moves into the shared one.
        let other = s.archive("other", &["Other"]);
        check(
            s.home.run(&[
                "install",
                "gh:o/other",
                "--file",
                &other,
                "-t",
                "anniversary",
            ]),
            0,
        );
        fs::rename(anniversary.join("Other"), era.join("Other")).unwrap();
        fs::remove_dir_all(&anniversary).unwrap();
        symlink(&era, &anniversary).unwrap();
        let both = s.both.display().to_string();
        check(s.home.run(&["install", &both, "-t", "era"]), 0);

        unpack(&root.join("src"), &[("Linked/Linked.toc", SHARED_TOC)]);
        symlink(root.join("src/Linked"), era.join("Linked")).unwrap();
        unpack(
            &era,
            &[
                ("Dup/Dup.toc", SHARED_TOC),
                ("dup/dup.toc", SHARED_TOC),
                ("respell/respell.toc", SHARED_TOC),
                ("Loose/Loose.toc", SHARED_TOC),
                ("Inner/Inner.toc", SHARED_TOC),
            ],
        );
        symlink(root.join("src/Both/Both.toc"), era.join("Inner/link.lua")).unwrap();
        s
    }

    /// `<name>.zip` in the download folder, one folder per name with its TOC; its path.
    fn archive(&self, name: &str, folders: &[&str]) -> String {
        let entries: Vec<(String, &str)> = folders
            .iter()
            .map(|f| (format!("{f}/{f}.toc"), SHARED_TOC))
            .collect();
        let entries: Vec<(&str, &str)> = entries.iter().map(|(p, t)| (p.as_str(), *t)).collect();
        let path = self.dl.join(format!("{name}.zip"));
        fs::write(&path, zip_text(&entries)).unwrap();
        path.display().to_string()
    }

    /// `install gh:o/multi` of an archive shipping `folders` beside `Multi` on
    /// Anniversary, refused (exit 4) with `code`.
    fn refused(&self, name: &str, folders: &[&str], code: ErrorCode) -> Checked {
        let mut all = vec!["Multi"];
        all.extend_from_slice(folders);
        let zip = self.archive(name, &all);
        let c = check(
            self.home
                .run(&["install", "gh:o/multi", "--file", &zip, "-t", "anniversary"]),
            4,
        );
        assert_eq!(
            c.envelope["error"]["code"],
            code.wire(),
            "{}",
            c.run.context()
        );
        c
    }
}

/// One install refused for every violation at once, then again with each top rank's
/// folder taken out: protected, then a symlink, then the case conflicts, then ownership.
const EVERY_RANK: Row = Row {
    id: "an_install_meeting_every_violation_is_refused_under_its_most_serious_code",
    keys: &[
        (ErrorCode::ProtectedFolder, "folders"),
        (ErrorCode::ProtectedFolder, "folders[].class"),
        (ErrorCode::ProtectedFolder, "folders[].code"),
        (ErrorCode::ProtectedFolder, "folders[].existing"),
        (ErrorCode::ProtectedFolder, "folders[].folder"),
        (ErrorCode::ProtectedFolder, "folders[].owner"),
        (ErrorCode::ProtectedFolder, "folders[].owner_target"),
        (ErrorCode::ProtectedFolder, "folders[].reason"),
        (ErrorCode::ProtectedFolder, "folders[].spellings"),
        (ErrorCode::UnmanagedFolder, "folders"),
        (ErrorCode::UnmanagedFolder, "folders[].class"),
        (ErrorCode::UnmanagedFolder, "folders[].code"),
        (ErrorCode::UnmanagedFolder, "folders[].existing"),
        (ErrorCode::UnmanagedFolder, "folders[].folder"),
        (ErrorCode::UnmanagedFolder, "folders[].owner"),
        (ErrorCode::UnmanagedFolder, "folders[].owner_target"),
        (ErrorCode::UnmanagedFolder, "folders[].reason"),
        (ErrorCode::UnmanagedFolder, "folders[].spellings"),
        (ErrorCode::CaseCollision, "folders"),
        (ErrorCode::CaseCollision, "folders[].code"),
        (ErrorCode::CaseCollision, "folders[].existing"),
        (ErrorCode::CaseCollision, "folders[].folder"),
        (ErrorCode::CaseCollision, "folders[].owner"),
        (ErrorCode::CaseCollision, "folders[].owner_target"),
        (ErrorCode::CaseCollision, "folders[].reason"),
        (ErrorCode::CaseCollision, "folders[].spellings"),
        (ErrorCode::OwnershipConflict, "folders"),
        (ErrorCode::OwnershipConflict, "folders[].code"),
        (ErrorCode::OwnershipConflict, "folders[].existing"),
        (ErrorCode::OwnershipConflict, "folders[].folder"),
        (ErrorCode::OwnershipConflict, "folders[].owner"),
        (ErrorCode::OwnershipConflict, "folders[].owner_target"),
        (ErrorCode::OwnershipConflict, "folders[].reason"),
    ],
    run: || {
        let s = Shared::new();
        let owned = ["Both", "Other", "Loose"];
        let case = ["Dup", "Respell"];
        let every: Vec<&str> = ["Blizzard_Multi", "Linked"]
            .into_iter()
            .chain(case)
            .chain(owned)
            .collect();
        vec![
            s.refused("protected", &every, ErrorCode::ProtectedFolder),
            s.refused("linked", &every[1..], ErrorCode::UnmanagedFolder),
            s.refused("case", &every[2..], ErrorCode::CaseCollision),
            s.refused("owned", &owned, ErrorCode::OwnershipConflict),
        ]
    },
};

/// Folders of the shared AddOns directory adoption and removal refuse: one Era records,
/// a symlink, one with a symlink inside, and an unmanaged one named for removal.
const NOT_ADOPTABLE: Row = Row {
    id: "folders_that_cannot_be_adopted_or_removed_say_why",
    keys: &[
        (ErrorCode::OwnershipConflict, "folder"),
        (ErrorCode::OwnershipConflict, "owner"),
        (ErrorCode::OwnershipConflict, "owner_target"),
        (ErrorCode::OwnershipConflict, "reason"),
        (ErrorCode::UnmanagedFolder, "class"),
        (ErrorCode::UnmanagedFolder, "folder"),
        (ErrorCode::Io, "folder"),
        (ErrorCode::Io, "path"),
        (ErrorCode::Io, "reason"),
        (ErrorCode::NotManaged, "class"),
        (ErrorCode::NotManaged, "folder"),
        (ErrorCode::NotManaged, "target"),
    ],
    run: || {
        let s = Shared::new();
        let file_ref = format!("file:{}", s.both.display());
        let run = |args: &[&str], code| check(s.home.run(args), code);
        vec![
            run(
                &[
                    "adopt",
                    "Both",
                    "-t",
                    "anniversary",
                    "--ref",
                    &file_ref,
                    "--no-verify",
                ],
                4,
            ),
            run(&["adopt", "Linked", "-t", "anniversary", "--no-verify"], 4),
            run(
                &[
                    "adopt",
                    "Inner",
                    "-t",
                    "anniversary",
                    "--ref",
                    "gh:o/inner",
                    "--no-verify",
                ],
                1,
            ),
            run(&["remove", "Loose", "-t", "anniversary"], 3),
        ]
    },
};

/// An install of a ref whose addon has the name of one recorded from another ref.
const TAKEOVER: Row = Row {
    id: "an_addon_of_the_same_name_from_another_ref_is_not_taken_over",
    keys: &[
        (ErrorCode::OwnershipConflict, "folders"),
        (ErrorCode::OwnershipConflict, "folders[].code"),
        (ErrorCode::OwnershipConflict, "folders[].folder"),
        (ErrorCode::OwnershipConflict, "folders[].owner"),
        (ErrorCode::OwnershipConflict, "folders[].reason"),
        (ErrorCode::OwnershipConflict, "folders[].ref"),
    ],
    run: || {
        let s = Scene::new();
        s.install_foo();
        let zip = s.zip("foo");
        vec![s.json(&["install", "gh:o/fork", "--file", &zip, "-t", "retail"], 4)]
    },
};

/// Two releases installed together that both ship one folder: the second fails.
const WITHIN_TRANSACTION: Row = Row {
    id: "two_releases_of_one_install_shipping_one_folder_conflict",
    keys: &[
        (ErrorCode::OwnershipConflict, "within_transaction"),
        (ErrorCode::OwnershipConflict, "folders"),
        (ErrorCode::OwnershipConflict, "folders[].folder"),
        (ErrorCode::OwnershipConflict, "folders[].owner"),
    ],
    run: || {
        let home = Home::new(&["wow"]);
        let retail = "## Interface: 120105\n## Version: v1\n";
        for (repo, folder) in [("s1", "S1"), ("s2", "S2")] {
            let (file, own) = (format!("{folder}.zip"), format!("{folder}/{folder}.toc"));
            let assets: [Asset<'_>; 1] = [(
                file.as_str(),
                &[("mainline", 120_105)],
                vec![(own.as_str(), retail), ("Shared/Shared.toc", retail)],
            )];
            gh_release(
                &home.http,
                &format!("https://github.com/o/{repo}"),
                "v1",
                &assets,
            );
        }
        let c = check(
            home.run(&["install", "gh:o/s1", "gh:o/s2", "-t", "retail"]),
            0,
        );
        vec![c]
    },
};

/// A named folder whose group's release installs it only on other clients.
const UNLOADED_GROUP: Row = Row {
    id: "a_named_folder_only_other_clients_load_is_not_adopted",
    keys: &[(ErrorCode::WrongFlavor, "unloaded_folders")],
    run: || {
        let home = Home::new(&["wow_classic"]);
        catalog(&home, "Foo", "gh:o/foo", &["Foo", "Foo_Retail"]);
        let foo = toc("50504, 120105", &["## Title: Foo", "## Version: v3"]);
        let retail = toc("120105", &["## Dependencies: Foo"]);
        let assets: [Asset<'_>; 1] = [(
            "Foo.zip",
            &[("mists", 50_504), ("mainline", 120_105)],
            vec![
                ("Foo/Foo.toc", foo.as_str()),
                ("Foo_Retail/Foo_Retail.toc", retail.as_str()),
            ],
        )];
        gh_release(&home.http, "https://github.com/o/foo", "v3", &assets);
        unpack(
            &home.addons("_classic_"),
            &[("Foo_Retail/Foo_Retail.toc", retail.as_str())],
        );
        vec![check(home.run(&["adopt", "Foo_Retail"]), 4)]
    },
};

/// A client whose `Interface/AddOns` has a second spelling differing only in case.
const TWO_ADDONS_SPELLINGS: Row = Row {
    id: "a_client_with_two_addons_spellings_is_refused",
    keys: &[(ErrorCode::CaseCollision, "paths")],
    run: || {
        let s = Scene::new();
        fs::create_dir_all(s.home.client("_retail_").join("Interface/addons")).unwrap();
        let zip = s.zip("foo");
        vec![s.json(&["install", "gh:o/foo", "--file", &zip, "-t", "retail"], 4)]
    },
};

/// A user catalog file naming one addon, `ref` and its `folders`.
fn catalog(home: &Home, name: &str, r#ref: &str, folders: &[&str]) {
    let folders: Vec<String> = folders.iter().map(|f| format!("{f:?}")).collect();
    let text = format!(
        "schema_version = \"1.0\"\n\n[[addon]]\nname = {name:?}\nref = {ref:?}\nfolders = [{}]\n",
        folders.join(", ")
    );
    let dir = home.env.config().join("omaddons/catalog.d");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("50-folders.toml"), text).unwrap();
}
