//! Every refused write leaves the tree untouched: a synthetic Wine prefix with a client
//! dir, protected folders, game files and SavedVariables, attacked with every op from
//! several writers. The fingerprint covers paths, kinds, modes, content and link
//! targets, so even a refused `CreateDir` or `Chmod` would show.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use omaddons::fsx::{
    Content, FsxError, Op, OwnedFolders, RenameMode, ScopeTable, SnapshotRoots, WriteSession,
    Writer, mutate,
};
use omaddons::model::{FolderName, TxId};
use omaddons::state::paths::Paths;

#[test]
fn refused_writes_change_nothing() {
    fn fingerprint(root: &Path) -> BTreeMap<PathBuf, String> {
        let mut out = BTreeMap::new();
        let mut pending = vec![root.to_path_buf()];
        while let Some(dir) = pending.pop() {
            for entry in fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                let meta = fs::symlink_metadata(&path).unwrap();
                let mode = meta.permissions().mode();
                let value = if meta.file_type().is_symlink() {
                    format!("link {mode:o} {}", fs::read_link(&path).unwrap().display())
                } else if meta.is_dir() {
                    pending.push(path.clone());
                    format!("dir {mode:o}")
                } else {
                    format!("file {mode:o} {:?}", fs::read(&path).unwrap())
                };
                out.insert(path.strip_prefix(root).unwrap().to_path_buf(), value);
            }
        }
        out
    }

    let dir = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();
    let home = root.join("home");
    let prefix = root.join("home/Games/wow-335");
    let client = prefix.join("drive_c/WoW");
    for d in [
        "Interface/AddOns/Blizzard_AuctionUI",
        "Interface/AddOns/WeakAuras",
        "Interface/.omaddons/stage/T",
        "Data/enUS",
        "WTF/Account/A/SavedVariables",
    ] {
        fs::create_dir_all(client.join(d)).unwrap();
    }
    fs::create_dir_all(prefix.join("drive_c/windows/system32")).unwrap();
    let paths = Paths::with_roots(
        &home,
        home.join(".config"),
        home.join(".local/state"),
        home.join(".cache"),
    );
    fs::create_dir_all(paths.state_dir()).unwrap();
    let files: &[(&str, &[u8])] = &[
        ("Wow.exe", b"MZ"),
        ("Data/data.001", b"MPQ"),
        ("Data/enUS/realmlist.wtf", b"set realmlist 127.0.0.1"),
        (
            "Interface/AddOns/Blizzard_AuctionUI/Blizzard_AuctionUI.toc",
            b"## Interface: 30300",
        ),
        ("WTF/Config.wtf", b"SET lastAddonVersion \"30300\""),
        (
            "WTF/Account/A/SavedVariables/WeakAuras.lua",
            b"WeakAurasSaved = {}",
        ),
        ("Interface/.omaddons/stage/T/staged.lua", b"-- staged"),
    ];
    for (rel, content) in files {
        fs::write(client.join(rel), content).unwrap();
    }
    fs::write(prefix.join("drive_c/windows/system32/kernel32.dll"), b"MZ").unwrap();
    fs::write(paths.lock_file(), b"").unwrap();

    let scopes = ScopeTable::new(&paths, std::slice::from_ref(&client), None).unwrap();
    let tx: TxId = "01JB8Q2VA00000000000000000".parse().unwrap();
    let before = fingerprint(&root);

    let addons = client.join("Interface/AddOns");
    let refused = [
        addons.join("Blizzard_AuctionUI"),
        addons.join("Blizzard_AuctionUI/Blizzard_AuctionUI.toc"),
        addons.join("BLIZZARD_AUCTIONUI/new.lua"),
        addons.join("blizzard_new"),
        paths.lock_file(),
        client.join("Wow.exe"),
        client.join("Data/data.001"),
        client.join("Data/new"),
        client.join("Data/enUS/realmlist.wtf"),
        client.join("WTF/Config.wtf"),
        client.join("WTF/Account/A/SavedVariables/WeakAuras.lua"),
        client.join("WTF/Account/A/SavedVariables/New.lua"),
        prefix.join("drive_c/windows/system32/kernel32.dll"),
        prefix.join("drive_c/new"),
        home.join("elsewhere"),
    ];
    let staged = client.join("Interface/.omaddons/stage/T/staged.lua");
    for writer in [
        Writer::Mutating,
        Writer::Doctor,
        Writer::Derived,
        Writer::Config,
        Writer::Undo,
    ] {
        let mut session = WriteSession::tx(
            tx,
            writer,
            &scopes,
            SnapshotRoots::for_tx(&paths, &tx),
            paths.state_snapshot(&tx).join("actions.jsonl"),
        );
        for target in &refused {
            let ops = [
                Op::WriteFile {
                    content: Content::Bytes(b"pwned"),
                    mode: None,
                },
                Op::AppendFile { content: b"pwned" },
                Op::CreateDir,
                Op::Chmod { mode: 0o777 },
                Op::SymlinkAtomic {
                    target: Path::new("/etc/passwd"),
                },
                Op::CloneFile {
                    from: &staged,
                    policy: omaddons::fsx::ClonePolicy::Preferred,
                },
            ];
            for op in ops {
                let wire = op.wire();
                let err = mutate(&mut session, target, op).unwrap_err();
                assert!(
                    !matches!(err, FsxError::Io { .. }),
                    "{writer} {wire} {}: refused late: {err}",
                    target.display()
                );
            }
            let err = mutate(
                &mut session,
                &staged,
                Op::Rename {
                    to: target,
                    mode: RenameMode::Replace,
                },
            )
            .unwrap_err();
            assert!(
                !matches!(err, FsxError::Io { .. }),
                "{writer} rename to {}: {err}",
                target.display()
            );
            if target.exists() {
                let away = client.join("Interface/.omaddons/stage/T/away");
                let err = mutate(
                    &mut session,
                    target,
                    Op::Rename {
                        to: &away,
                        mode: RenameMode::NoReplace,
                    },
                )
                .unwrap_err();
                assert!(
                    !matches!(err, FsxError::Io { .. }),
                    "{writer} rename of {}: {err}",
                    target.display()
                );
            }
        }
    }
    assert_eq!(
        fingerprint(&root),
        before,
        "a refused write changed the tree"
    );
}

/// A `WTF/` symlinked to a sync folder outside the client stays out of the Undo writer's
/// reach under either name, even for a folder it owns; the client's own `WTF/` does not.
#[test]
fn the_undo_writer_never_follows_wtf_out_of_the_client() {
    let dir = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();
    let home = root.join("home");
    let client = home.join("Games/WoW/_retail_");
    let sync = root.join("sync/WTF");
    let sv = "Account/A/SavedVariables/Foo.lua";
    fs::create_dir_all(client.join("Interface/AddOns")).unwrap();
    fs::create_dir_all(sync.join(sv).parent().unwrap()).unwrap();
    fs::write(sync.join(sv), b"FooDB = {}").unwrap();
    std::os::unix::fs::symlink(&sync, client.join("WTF")).unwrap();
    let paths = Paths::with_roots(
        &home,
        home.join(".config"),
        home.join(".local/state"),
        home.join(".cache"),
    );
    fs::create_dir_all(paths.state_dir()).unwrap();
    let scopes = ScopeTable::new(&paths, std::slice::from_ref(&client), None).unwrap();
    let tx = TxId::new();
    let foo = FolderName::new("Foo").unwrap();
    let mut session = WriteSession::tx(
        tx,
        Writer::Undo,
        &scopes,
        SnapshotRoots::for_tx(&paths, &tx),
        paths.state_snapshot(&tx).join("actions.jsonl"),
    )
    .with_owned_folders(OwnedFolders::new(std::slice::from_ref(&foo)));
    let write = || Op::WriteFile {
        content: Content::Bytes(b"pwned"),
        mode: None,
    };
    for target in [client.join("WTF").join(sv), sync.join(sv)] {
        let err = mutate(&mut session, &target, write()).unwrap_err();
        assert!(
            matches!(err, FsxError::ScopeDenied(_)),
            "{}: {err}",
            target.display()
        );
    }
    assert_eq!(fs::read(sync.join(sv)).unwrap(), b"FooDB = {}");

    // The client's own WTF/ is the Undo writer's, for the folder it owns.
    fs::remove_file(client.join("WTF")).unwrap();
    fs::create_dir_all(client.join("WTF").join(sv).parent().unwrap()).unwrap();
    mutate(&mut session, &client.join("WTF").join(sv), write()).unwrap();
}
