//! e2e suite: fsx endpoint predicates and canonical resources through real write sessions.
//!
//! The predicates (`fsx::Expect`, `src/fsx/expect.rs`) are checked at `mutate()`'s first
//! step, on an operation's own path and on a rename's destination. Here each part of a
//! predicate is captured on a synthetic home, the endpoint is changed in the one way that
//! part sees, and the operation must be refused as a precondition naming that endpoint and
//! part, leaving the tree, the actions log and the snapshot roots as the change left
//! them. With nothing changed, every operation goes through and logs one action. The
//! resource model (`engine::resource`, `src/engine/resource.rs`) must canonicalize every
//! spelling of a path to exactly the path `mutate()` logs, and overlap across a symlinked
//! home. No binary runs: nothing on the command line uses these predicates yet.

// The homes and the changes are test setup: a failed write is the test's failure.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::fs::{self, File};
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use common::tlog::TestLog;
use common::trees::tree_hash;
use omaddons::engine::resource::{Access, Conflict, PathKind, Resource, ResourceSet};
use omaddons::fsx::{
    ActionRecord, Content, Expect, FileId, FileKind, FsxError, MutateOpts, Op, Parts, Predicate,
    RenameMode, ScopeTable, SnapshotRoots, WriteSession, Writer, mutate, mutate_with,
};
use omaddons::model::TxId;
use omaddons::state::paths::Paths;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use tempfile::TempDir;

const SUITE: &str = "fsx_predicates";
const TX: &str = "01JB8Q2VA00000000000000000";

/// A synthetic home: `<home>/.local/state`, and a client at `<home>/Games/wow/_retail_`
/// with `Interface/AddOns/{X,Y}`, each holding a `.toc` and a `.lua`.
struct Home {
    _dir: TempDir,
    root: PathBuf,
    /// The home's real directory.
    real_home: PathBuf,
    /// The home as `Paths` and the client were given it: through `<root>/link-home` for
    /// [`Home::linked`].
    home: PathBuf,
    paths: Paths,
    client: PathBuf,
    scopes: ScopeTable,
    tx: TxId,
}

impl Home {
    fn new() -> Self {
        Self::build(false)
    }

    /// The same tree, reached through `<root>/link-home`, a symlink to `<root>/real-home`.
    fn linked() -> Self {
        Self::build(true)
    }

    fn build(linked: bool) -> Self {
        // Under the target's tmp dir, so the log shows its paths as `<target-tmp>/…`.
        let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        let real_home = root.join(if linked { "real-home" } else { "home" });
        fs::create_dir_all(&real_home).unwrap();
        let home = if linked {
            let link = root.join("link-home");
            symlink(&real_home, &link).unwrap();
            link
        } else {
            real_home.clone()
        };
        let paths = Paths::with_roots(
            &home,
            home.join(".config"),
            home.join(".local/state"),
            home.join(".cache"),
        );
        let client = home.join("Games/wow/_retail_");
        for folder in ["X", "Y"] {
            let dir = client.join("Interface/AddOns").join(folder);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join(format!("{folder}.toc")), "## Interface: 120105\n").unwrap();
            fs::write(dir.join(format!("{folder}.lua")), format!("-- {folder}\n")).unwrap();
        }
        fs::write(client.join("Wow.exe"), b"MZ").unwrap();
        fs::create_dir_all(paths.state_dir()).unwrap();
        let scopes = ScopeTable::new(&paths, std::slice::from_ref(&client), None).unwrap();
        Self {
            _dir: dir,
            root,
            real_home,
            home,
            paths,
            client,
            scopes,
            tx: TX.parse().unwrap(),
        }
    }

    fn session(&self) -> WriteSession<'_> {
        WriteSession::tx(
            self.tx,
            Writer::Mutating,
            &self.scopes,
            SnapshotRoots::for_tx(&self.paths, &self.tx),
            self.actions_log(),
        )
    }

    fn actions_log(&self) -> PathBuf {
        self.paths.state_snapshot(&self.tx).join("actions.jsonl")
    }

    fn actions(&self) -> Vec<Value> {
        fs::read_to_string(self.actions_log())
            .map(|text| {
                text.lines()
                    .map(|line| serde_json::from_str(line).unwrap())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn addons(&self) -> PathBuf {
        self.client.join("Interface/AddOns")
    }

    fn state(&self) -> PathBuf {
        self.paths.state_dir().to_path_buf()
    }

    /// `path` below the temp root, spelled as given (a trailing slash or `..` kept).
    fn rel(&self, path: &Path) -> String {
        let (path, root) = (path.display().to_string(), self.root.display().to_string());
        path.strip_prefix(&format!("{root}/"))
            .map_or_else(|| path.clone(), str::to_owned)
    }
}

fn sha(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn capture(path: &Path, parts: Parts) -> Expect {
    Expect::capture(path, parts).unwrap()
}

fn identity() -> Parts {
    Parts {
        identity: true,
        ..Parts::default()
    }
}

fn file(path: &Path, bytes: &[u8], mode: u32) {
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

/// Another file with the same bytes and mode, renamed over `path`: only its identity
/// changes.
fn replace_file(path: &Path) {
    let copy = path.with_extension("copy");
    fs::copy(path, &copy).unwrap();
    fs::rename(&copy, path).unwrap();
}

/// A copy of the folder at `path` (same names, bytes and modes) put in its place; the
/// original is moved to `<root>/aside/`.
fn replace_dir(root: &Path, path: &Path) {
    fn copy_tree(from: &Path, to: &Path) {
        fs::create_dir(to).unwrap();
        for entry in fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            let (src, dst) = (entry.path(), to.join(entry.file_name()));
            if entry.file_type().unwrap().is_dir() {
                copy_tree(&src, &dst);
            } else {
                fs::copy(&src, &dst).unwrap();
            }
        }
        let mode = fs::metadata(from).unwrap().permissions().mode();
        fs::set_permissions(to, fs::Permissions::from_mode(mode)).unwrap();
    }
    let name = path.file_name().unwrap();
    let copy = root.join("copy").join(name);
    fs::create_dir_all(copy.parent().unwrap()).unwrap();
    copy_tree(path, &copy);
    let aside = root.join("aside");
    fs::create_dir_all(&aside).unwrap();
    fs::rename(path, aside.join(name)).unwrap();
    fs::rename(&copy, path).unwrap();
}

/// Rewrites `path` in place with `bytes` (the same size), its mtime put back.
fn rewrite_keeping_mtime(path: &Path, bytes: &[u8]) {
    let mtime: SystemTime = fs::metadata(path).unwrap().modified().unwrap();
    fs::write(path, bytes).unwrap();
    File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(mtime)
        .unwrap();
}

/// The operation a cell runs, and so its endpoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// `WriteFile` of `<state>/subject`.
    Write,
    /// `Chmod 0600` of `<state>/subject`.
    Chmod,
    /// `AddOns/X` renamed to `AddOns/X-moved`, never replacing.
    NoReplace,
    /// `<state>/a` renamed over `<state>/b`.
    Replace,
    /// `AddOns/X` exchanged with `AddOns/Y`.
    Exchange,
    /// `<state>/latest` pointed at `<state>/elsewhere`.
    Symlink,
}

/// Which endpoint a cell's predicate is on: `expect` on the operation's own path, or
/// `expect_to` on a rename's destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum End {
    Path,
    To,
}

fn endpoints(h: &Home, kind: Kind) -> (PathBuf, Option<PathBuf>) {
    match kind {
        Kind::Write | Kind::Chmod => (h.state().join("subject"), None),
        Kind::NoReplace => (h.addons().join("X"), Some(h.addons().join("X-moved"))),
        Kind::Replace => (h.state().join("a"), Some(h.state().join("b"))),
        Kind::Exchange => (h.addons().join("X"), Some(h.addons().join("Y"))),
        Kind::Symlink => (h.state().join("latest"), None),
    }
}

fn operate(
    h: &Home,
    s: &mut WriteSession<'_>,
    kind: Kind,
    opts: MutateOpts,
) -> (&'static str, Result<ActionRecord, FsxError>) {
    let (path, to) = endpoints(h, kind);
    let elsewhere = h.state().join("elsewhere");
    let dest = to.unwrap_or_default();
    let op = match kind {
        Kind::Write => Op::WriteFile {
            content: Content::Bytes(b"written"),
            mode: None,
        },
        Kind::Chmod => Op::Chmod { mode: 0o600 },
        Kind::NoReplace => Op::Rename {
            to: &dest,
            mode: RenameMode::NoReplace,
        },
        Kind::Replace => Op::Rename {
            to: &dest,
            mode: RenameMode::Replace,
        },
        Kind::Exchange => Op::Rename {
            to: &dest,
            mode: RenameMode::Exchange,
        },
        Kind::Symlink => Op::SymlinkAtomic { target: &elsewhere },
    };
    (op.wire(), mutate_with(s, &path, op, opts))
}

/// One row of the matrix: an operation, the endpoint its predicate is on, how the
/// subject is built, the predicate, the change that part must see, and the refusal.
struct Cell {
    name: &'static str,
    kind: Kind,
    end: End,
    build: fn(&Home),
    expect: fn(&Home, &Path) -> Expect,
    change: fn(&Home),
    /// The part that refuses (a `Mismatch::part`, or `expect_to` for a misplaced one).
    part: &'static str,
    /// How the refusal message ends.
    refusal: &'static str,
}

fn subject(h: &Home) -> PathBuf {
    h.state().join("subject")
}

fn cells() -> Vec<Cell> {
    let nothing: fn(&Home) = |_| {};
    let a_file: fn(&Home) = |h| file(&subject(h), b"v1", 0o644);
    vec![
        Cell {
            name: "write_identity",
            kind: Kind::Write,
            end: End::Path,
            build: a_file,
            expect: |_, p| capture(p, identity()),
            change: |h| replace_file(&subject(h)),
            part: "identity",
            refusal: "another file is at this path (its identity changed)",
        },
        Cell {
            name: "write_sha256",
            kind: Kind::Write,
            end: End::Path,
            build: |h| file(&subject(h), b"aaaa", 0o644),
            expect: |_, _| Expect::hash(sha(b"aaaa")),
            change: |h| rewrite_keeping_mtime(&subject(h), b"aaab"),
            part: "sha256",
            refusal: "its content changed (hash mismatch)",
        },
        Cell {
            name: "write_mode",
            kind: Kind::Write,
            end: End::Path,
            build: a_file,
            expect: |_, p| {
                let parts = Parts {
                    mode: true,
                    ..Parts::default()
                };
                capture(p, parts)
            },
            change: |h| {
                fs::set_permissions(subject(h), fs::Permissions::from_mode(0o600)).unwrap();
            },
            part: "mode",
            refusal: "its mode is 0600, expected 0644",
        },
        Cell {
            name: "write_absent",
            kind: Kind::Write,
            end: End::Path,
            build: nothing,
            expect: |_, _| Expect::Absent,
            change: |h| file(&subject(h), b"another writer", 0o644),
            part: "absent",
            refusal: "expected nothing at this path",
        },
        Cell {
            name: "write_present",
            kind: Kind::Write,
            end: End::Path,
            build: a_file,
            expect: |_, _| Expect::Present(Predicate::default()),
            change: |h| fs::remove_file(subject(h)).unwrap(),
            part: "present",
            refusal: "expected something at this path",
        },
        Cell {
            name: "chmod_identity",
            kind: Kind::Chmod,
            end: End::Path,
            build: a_file,
            expect: |_, p| {
                let parts = Parts {
                    identity: true,
                    mode: true,
                    ..Parts::default()
                };
                capture(p, parts)
            },
            change: |h| replace_file(&subject(h)),
            part: "identity",
            refusal: "another file is at this path (its identity changed)",
        },
        Cell {
            name: "chmod_mode",
            kind: Kind::Chmod,
            end: End::Path,
            build: a_file,
            expect: |_, p| {
                let parts = Parts {
                    identity: true,
                    mode: true,
                    ..Parts::default()
                };
                capture(p, parts)
            },
            change: |h| {
                fs::set_permissions(subject(h), fs::Permissions::from_mode(0o640)).unwrap();
            },
            part: "mode",
            refusal: "its mode is 0640, expected 0644",
        },
        Cell {
            name: "noreplace_from_identity",
            kind: Kind::NoReplace,
            end: End::Path,
            build: nothing,
            expect: |_, p| capture(p, identity()),
            change: |h| replace_dir(&h.root, &h.addons().join("X")),
            part: "identity",
            refusal: "another file is at this path (its identity changed)",
        },
        Cell {
            name: "noreplace_from_stamp",
            kind: Kind::NoReplace,
            end: End::Path,
            build: nothing,
            expect: |_, p| {
                let parts = Parts {
                    stamp: true,
                    ..Parts::default()
                };
                capture(p, parts)
            },
            change: |h| file(&h.addons().join("X/added.lua"), b"-- added\n", 0o644),
            part: "stamp",
            refusal: "the directory's contents changed (stamp mismatch)",
        },
        Cell {
            name: "noreplace_from_type",
            kind: Kind::NoReplace,
            end: End::Path,
            build: nothing,
            expect: |_, _| {
                Expect::Present(Predicate {
                    file_type: Some(FileKind::Directory),
                    ..Predicate::default()
                })
            },
            change: |h| {
                let x = h.addons().join("X");
                fs::rename(&x, h.root.join("X-aside")).unwrap();
                file(&x, b"a file now", 0o644);
            },
            part: "file_type",
            refusal: "is a regular file, expected a directory",
        },
        Cell {
            name: "replace_to_absent",
            kind: Kind::Replace,
            end: End::To,
            build: |h| file(&h.state().join("a"), b"A", 0o644),
            expect: |_, _| Expect::Absent,
            change: |h| file(&h.state().join("b"), b"B", 0o644),
            part: "absent",
            refusal: "expected nothing at this path",
        },
        Cell {
            name: "exchange_to_identity",
            kind: Kind::Exchange,
            end: End::To,
            build: nothing,
            expect: |_, p| capture(p, identity()),
            change: |h| replace_dir(&h.root, &h.addons().join("Y")),
            part: "identity",
            refusal: "another file is at this path (its identity changed)",
        },
        Cell {
            name: "exchange_from_identity",
            kind: Kind::Exchange,
            end: End::Path,
            build: nothing,
            expect: |_, p| capture(p, identity()),
            change: |h| replace_dir(&h.root, &h.addons().join("X")),
            part: "identity",
            refusal: "another file is at this path (its identity changed)",
        },
        Cell {
            name: "replace_to_sha256",
            kind: Kind::Replace,
            end: End::To,
            build: |h| {
                file(&h.state().join("a"), b"new", 0o644);
                file(&h.state().join("b"), b"bbbb", 0o644);
            },
            expect: |_, _| Expect::hash(sha(b"bbbb")),
            change: |h| rewrite_keeping_mtime(&h.state().join("b"), b"bbbc"),
            part: "sha256",
            refusal: "its content changed (hash mismatch)",
        },
        Cell {
            name: "symlink_link_target",
            kind: Kind::Symlink,
            end: End::Path,
            build: |h| symlink("first", h.state().join("latest")).unwrap(),
            expect: |_, p| {
                let parts = Parts {
                    link_target: true,
                    ..Parts::default()
                };
                capture(p, parts)
            },
            change: |h| {
                let link = h.state().join("latest");
                fs::remove_file(&link).unwrap();
                symlink("second", &link).unwrap();
            },
            part: "link_target",
            refusal: "the symlink points elsewhere",
        },
        Cell {
            name: "write_identity_renumbered_device_still_refuses_another_file",
            kind: Kind::Write,
            end: End::Path,
            build: a_file,
            expect: |_, p| match capture(p, identity()) {
                Expect::Present(mut predicate) => {
                    if let Some(id) = predicate.identity.as_mut() {
                        id.dev ^= 0x5a5a;
                    }
                    Expect::Present(predicate)
                }
                Expect::Absent => Expect::Absent,
            },
            change: |h| replace_file(&subject(h)),
            part: "identity",
            refusal: "another file is at this path (its identity changed)",
        },
        Cell {
            name: "destination_predicate_on_a_write",
            kind: Kind::Write,
            end: End::To,
            build: nothing,
            expect: |_, _| Expect::Absent,
            change: nothing,
            part: "expect_to",
            refusal: "a destination predicate needs a rename",
        },
    ]
}

/// The cell's predicate on its endpoint, and the path a refusal names: the endpoint, or
/// the operation's own path when the operation has no destination.
fn predicate(h: &Home, cell: &Cell) -> (MutateOpts, PathBuf) {
    let (path, to) = endpoints(h, cell.kind);
    let (on, named) = match (cell.end, to) {
        (End::To, Some(to)) => (to.clone(), to),
        _ => (path.clone(), path),
    };
    let expect = (cell.expect)(h, &on);
    let opts = match cell.end {
        End::Path => MutateOpts {
            expect: Some(expect),
            ..MutateOpts::default()
        },
        End::To => MutateOpts {
            expect_to: Some(expect),
            ..MutateOpts::default()
        },
    };
    (opts, named)
}

fn identities(h: &Home, kind: Kind) -> Vec<(String, Option<FileId>)> {
    let (path, to) = endpoints(h, kind);
    [Some(path), to]
        .into_iter()
        .flatten()
        .map(|p| (h.rel(&p), FileId::of(&p)))
        .collect()
}

#[test]
fn every_predicate_part_refuses_its_own_change() {
    let log = TestLog::new(SUITE, "every_predicate_part_refuses_its_own_change");
    for cell in cells() {
        let h = Home::new();
        let mut s = h.session();
        log.step(&format!("{}: build", cell.name));
        (cell.build)(&h);
        let (opts, named) = predicate(&h, &cell);
        log.note(format!(
            "expect {:?}, expect_to {:?}",
            opts.expect, opts.expect_to
        ));
        log.json("endpoints", &identities(&h, cell.kind));
        log.tree("subject", &h.root);

        log.step(&format!("{}: change", cell.name));
        (cell.change)(&h);
        log.note(format!("changed: {}", cell.part));
        log.tree("changed", &h.root);
        let after_change = identities(&h, cell.kind);
        let lines = h.actions().len();

        log.step(&format!("{}: operate", cell.name));
        let (_, got) = operate(&h, &mut s, cell.kind, opts);
        let error = got.as_ref().err().map(ToString::to_string);
        log.json(
            "result",
            &json!({"ok": got.is_ok(), "error": error, "part": cell.part}),
        );
        let refused = matches!(&got, Err(FsxError::Precondition(message))
            if message.starts_with(&*named.to_string_lossy()) && message.ends_with(cell.refusal));
        assert!(
            refused,
            "{}: {got:?}, expected a precondition on {} ending {:?}",
            cell.name,
            h.rel(&named),
            cell.refusal
        );
        log.assert_unchanged("changed", &h.root);
        log.json(
            "actions_lines",
            &json!({"before": lines, "after": h.actions().len()}),
        );
        assert_eq!(h.actions().len(), lines, "{}: no actions line", cell.name);
        log.assert_eq(
            &format!("{}: endpoint identities", cell.name),
            &identities(&h, cell.kind),
            &after_change,
        );
    }
}

#[test]
fn an_unchanged_path_passes_every_part() {
    let log = TestLog::new(SUITE, "an_unchanged_path_passes_every_part");
    for cell in cells()
        .into_iter()
        .filter(|cell| cell.name != "destination_predicate_on_a_write")
    {
        let h = Home::new();
        let mut s = h.session();
        log.step(&format!("{}: build", cell.name));
        (cell.build)(&h);
        let (opts, _) = predicate(&h, &cell);
        log.note(format!(
            "expect {:?}, expect_to {:?}",
            opts.expect, opts.expect_to
        ));
        let lines = h.actions().len();

        log.step(&format!("{}: operate", cell.name));
        let (wire, got) = operate(&h, &mut s, cell.kind, opts);
        let error = got.as_ref().err().map(ToString::to_string);
        log.json("result", &json!({"ok": got.is_ok(), "error": error}));
        assert!(got.is_ok(), "{}: {got:?}", cell.name);
        let actions = h.actions();
        log.json(
            "actions_lines",
            &json!({"before": lines, "after": actions.len()}),
        );
        assert_eq!(actions.len(), lines + 1, "{}: one action logged", cell.name);
        assert_eq!(
            actions.last().and_then(|a| a["op"].as_str()),
            Some(wire),
            "{}: the logged op",
            cell.name
        );
    }
}

#[test]
fn identity_survives_a_renumbered_device() {
    let log = TestLog::new(SUITE, "identity_survives_a_renumbered_device");
    let h = Home::new();
    let mut s = h.session();
    let x = h.addons().join("X");
    log.step("capture X with its device rewritten");
    let mut id = FileId::of(&x).unwrap();
    assert!(
        capture(&x, identity())
            == Expect::Present(Predicate {
                file_type: Some(FileKind::Directory),
                identity: Some(id),
                ..Predicate::default()
            }),
        "the captured identity is FileId::of's"
    );
    id.dev ^= 0x5a5a;
    let predicate = Predicate {
        identity: Some(id),
        ..Predicate::default()
    };
    log.note(format!("{predicate:?}"));
    log.step("rename X");
    let opts = MutateOpts {
        expect: Some(Expect::Present(predicate)),
        expect_to: Some(Expect::Absent),
        ..MutateOpts::default()
    };
    let (_, got) = operate(&h, &mut s, Kind::NoReplace, opts);
    let error = got.as_ref().err().map(ToString::to_string);
    log.json("result", &json!({"ok": got.is_ok(), "error": error}));
    assert!(got.is_ok(), "{got:?}");
    assert!(h.addons().join("X-moved/X.toc").is_file());
}

#[test]
fn a_rename_checks_both_endpoints() {
    let log = TestLog::new(SUITE, "a_rename_checks_both_endpoints");
    let h = Home::new();
    let mut s = h.session();
    let (x, y) = (h.addons().join("X"), h.addons().join("Y"));
    let both = |h: &Home| MutateOpts {
        expect: Some(capture(&h.addons().join("X"), identity())),
        expect_to: Some(capture(&h.addons().join("Y"), identity())),
        ..MutateOpts::default()
    };

    log.step("capture both, then replace Y");
    let stale = both(&h);
    replace_dir(&h.root, &y);
    log.tree("changed", &h.root);
    let ids = identities(&h, Kind::Exchange);
    log.json("endpoints", &ids);
    log.step("exchange with a stale destination");
    let (_, got) = operate(&h, &mut s, Kind::Exchange, stale);
    let error = got.as_ref().err().map(ToString::to_string);
    log.json("result", &json!({"ok": got.is_ok(), "error": error}));
    assert!(
        matches!(&got, Err(FsxError::Precondition(message))
            if message.starts_with(&*y.to_string_lossy())
                && message.ends_with("(its identity changed)")),
        "{got:?}"
    );
    log.assert_unchanged("changed", &h.root);
    log.assert_eq("endpoint identities", &identities(&h, Kind::Exchange), &ids);

    log.step("exchange with fresh captures");
    let (x_tree, y_tree) = (tree_hash(&x), tree_hash(&y));
    let (_, got) = operate(&h, &mut s, Kind::Exchange, both(&h));
    let error = got.as_ref().err().map(ToString::to_string);
    log.json("result", &json!({"ok": got.is_ok(), "error": error}));
    assert!(got.is_ok(), "{got:?}");
    log.assert_eq(
        "the trees swapped",
        &(tree_hash(&x), tree_hash(&y)),
        &(y_tree, x_tree),
    );
}

#[test]
fn resources_canonicalize_as_fsx_normalizes() {
    let log = TestLog::new(SUITE, "resources_canonicalize_as_fsx_normalizes");
    let h = Home::linked();
    let mut s = h.session();
    let x = h.addons().join("X");
    let through = |rest: &str| PathBuf::from(format!("{}/{rest}", h.home.display()));
    // (spelling, the resource's input, the same path as mutate() is given it: no `..`)
    let spellings = [
        ("through_the_link", x.join("a.lua"), x.join("a.lua")),
        (
            "dotdot_through_an_existing_directory",
            h.addons().join("Y/../X/b.lua"),
            x.join("b.lua"),
        ),
        (
            "dot",
            through("Games/./wow/_retail_/Interface/AddOns/X/c.lua"),
            through("Games/./wow/_retail_/Interface/AddOns/X/c.lua"),
        ),
        (
            "trailing_slash",
            through("Games/wow/_retail_/Interface/AddOns/X/d.lua/"),
            through("Games/wow/_retail_/Interface/AddOns/X/d.lua/"),
        ),
    ];
    for (spelling, input, written) in &spellings {
        log.step(spelling);
        let resource = Resource::canonical(input, PathKind::File).unwrap();
        let record = mutate(
            &mut s,
            written,
            Op::WriteFile {
                content: Content::Bytes(b"-- x\n"),
                mode: None,
            },
        )
        .unwrap();
        log.json(
            "spelling",
            &json!({"input": h.rel(input), "resource": resource.text().unwrap(), "action_path": record.path}),
        );
        assert_eq!(
            resource.path().and_then(Path::to_str),
            Some(record.path.as_str()),
            "{spelling}: {input:?}"
        );
        assert!(
            resource.overlaps_path(Path::new(&record.path)),
            "{spelling}: {resource} overlaps {}",
            record.path
        );
        assert!(
            Path::new(&record.path).starts_with(&h.real_home),
            "{spelling}: logged under the real home"
        );
    }

    log.step("final_symlink");
    let link = h.state().join("latest-like");
    symlink("somewhere", &link).unwrap();
    let resource = Resource::canonical(&link, PathKind::File).unwrap();
    let target = h.state().join("elsewhere");
    let record = mutate(&mut s, &link, Op::SymlinkAtomic { target: &target }).unwrap();
    log.json(
        "spelling",
        &json!({"input": h.rel(&link), "resource": resource.text().unwrap(), "action_path": record.path}),
    );
    assert_eq!(
        resource.path().and_then(Path::to_str),
        Some(record.path.as_str()),
        "final symlink: the link's own path"
    );
}

#[test]
fn conflicts_hold_across_spellings() {
    let log = TestLog::new(SUITE, "conflicts_hold_across_spellings");
    let h = Home::linked();
    let write = |path: PathBuf, kind: PathKind| {
        let mut set = ResourceSet::new();
        set.write(Resource::canonical(&path, kind).unwrap());
        set
    };
    let quarantine = write(h.real_home.join("x"), PathKind::Subtree);
    let child = write(h.home.join("x/y"), PathKind::File);
    let sibling = write(h.home.join("xy"), PathKind::File);
    let text = |conflicts: &[Conflict]| -> Vec<(String, String)> {
        conflicts
            .iter()
            .map(|c| (c.ours.text().unwrap(), c.theirs.text().unwrap()))
            .collect()
    };

    log.step("a quarantine against a write into its child");
    let got = quarantine.conflicts(&child);
    log.json("conflicts", &text(&got));
    let want = vec![Conflict {
        ours: Resource::from_canonical(&h.real_home.join("x"), PathKind::Subtree).unwrap(),
        ours_access: Access::Write,
        theirs: Resource::from_canonical(&h.real_home.join("x/y"), PathKind::File).unwrap(),
        theirs_access: Access::Write,
    }];
    assert_eq!(got, want, "the conflict names both canonical resources");

    log.step("a quarantine against a sibling");
    let got = quarantine.conflicts(&sibling);
    log.json("conflicts", &text(&got));
    assert!(got.is_empty(), "{got:?}");
}
