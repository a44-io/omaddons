//! Bound runs commit exactly what their preview approved, or nothing (M4-D60), through the
//! real binary: the approval witness and the re-judged operations checked again under the
//! commit lock (D8 part 1, `omad-m4-agent-surface-doctor-c6cm.4.8.1`), and each rename's
//! identity (part 2, `omad-m4-agent-surface-doctor-c6cm.4.8.2`).
//!
//! Every interleaving is forced by a `pause` failpoint in the commit window
//! (`src/engine/tx.rs`'s `locked`): `engine.snapshot.prestaged` comes after the bound
//! run's own first check and its staging, before the lock (a "late" edit);
//! `engine.registered` holds the lock, before the under-lock check; `engine.revalidated`
//! comes after both under-lock checks, before the swap. The scenarios drive typed
//! `--if-match` runs, the bound path `apply` will use (D5,
//! `omad-m4-agent-surface-doctor-c6cm.4.5`).

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::common::binding as b;
use crate::common::engine::folder;
use crate::common::graph::{Folder, Node, release_graph};
use crate::common::home::Home;
use crate::common::http::{Asset, gh_release};
use crate::common::pause::Pause;
use crate::common::tlog::{Ran, TestLog};
use crate::common::trees::{copy_tree_exact, tree_hash};
use crate::common::wtf::Wtf;
use crate::common::{addons, state};

/// The `.lua` files of a witnessed folder, besides its TOC.
const FILES: usize = 12;
/// The folder Foo v2 adds: a name the plan creates.
const EXTRA: &str = "Foo_Extra";
/// The interfaces every TOC here lists: retail, Mists Classic and Classic Era.
const INTERFACES: &str = "120105, 50504, 11509";
/// The pause points of the commit window.
const PRESTAGED: &str = "engine.snapshot.prestaged";
const REGISTERED: &str = "engine.registered";
const REVALIDATED: &str = "engine.revalidated";
/// The `-v` note each checked window adds.
const COST_NOTE: &str = "approval checked under the lock";

fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|a| (*a).to_owned()).collect()
}

/// `args` with the presentation flags every run here takes.
fn json(args: &[String]) -> Vec<String> {
    let mut argv = args.to_vec();
    argv.extend(strings(&["--json", "--progress", "json"]));
    argv
}

/// Runs `args` as given, and checks that neither stream names an account, realm or
/// character (every home here has SavedVariables under the fake names).
fn run(log: &TestLog, home: &Home, args: &[String]) -> Ran {
    let ran = log.run(home, args);
    ran.assert_private();
    ran
}

/// `omaddons <args> --json --progress json`, which must succeed.
fn ok(log: &TestLog, home: &Home, args: &[&str]) -> Ran {
    let ran = run(log, home, &json(&strings(args)));
    ran.ok();
    ran
}

/// SavedVariables of `folders` on `client`, under the fake account, realm and character.
fn saved_variables(home: &Home, client: &str, folders: &[&str]) {
    let mut wtf = Wtf::at(home.client(client));
    for name in folders {
        wtf = wtf.saved_variables(name, 256);
    }
    wtf.build();
}

/// A release's files: `<name>/<name>.toc`, `<name>/f00.lua` … `f11.lua`, then each of
/// `extra` as a folder holding its TOC only.
fn entries(name: &str, tag: &str, extra: &[&str]) -> Vec<(String, String)> {
    let mut files = vec![(
        format!("{name}/{name}.toc"),
        format!("## Interface: {INTERFACES}\n## Title: {name}\n## Version: {tag}\n"),
    )];
    files.extend((0..FILES).map(|i| (format!("{name}/f{i:02}.lua"), format!("-- {tag} {i}\n"))));
    files.extend(extra.iter().map(|x| {
        (
            format!("{x}/{x}.toc"),
            format!("## Interface: {INTERFACES}\n## Version: {tag}\n"),
        )
    }));
    files
}

/// Release `tag` of `gh:o/<repo>`, shipping [`entries`], as its latest. Its release.json
/// names no flavor, so the TOCs decide.
fn publish(home: &Home, repo: &str, name: &str, tag: &str, extra: &[&str]) {
    let files = entries(name, tag, extra);
    let borrowed: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, t)| (p.as_str(), t.as_str()))
        .collect();
    let file = format!("{name}.zip");
    let assets: [Asset<'_>; 1] = [(file.as_str(), &[], borrowed)];
    gh_release(
        &home.http,
        &format!("https://github.com/o/{repo}"),
        tag,
        &assets,
    );
}

/// Release `tag` of `gh:o/foo`: `Foo` and each of `extra`.
fn foo_release(home: &Home, tag: &str, extra: &[&str]) {
    publish(home, "foo", "Foo", tag, extra);
}

/// The same archive as a local zip `<dl>/<name>`; returns its path.
fn foo_zip(dl: &Path, name: &str, tag: &str, extra: &[&str]) -> String {
    fs::create_dir_all(dl).unwrap();
    let files = entries("Foo", tag, extra);
    let borrowed: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, t)| (p.as_str(), t.as_str()))
        .collect();
    let path = dl.join(name);
    fs::write(&path, crate::common::archives::zip_text(&borrowed)).unwrap();
    path.display().to_string()
}

/// The tree hash of `root`, `None` when it is absent.
fn hash(root: &Path) -> Option<String> {
    fs::symlink_metadata(root).is_ok().then(|| tree_hash(root))
}

/// The transactions `history.jsonl` names, oldest first.
fn history_txs(home: &Home) -> Vec<String> {
    fs::read_to_string(home.state().join("history.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .map(|v| v["tx"].as_str().unwrap_or_default().to_owned())
        .collect()
}

fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .map(|l| {
            l.map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// What a refused run must not leave behind: a journal, a stage, or a snapshot (either
/// half) of a transaction history does not name.
fn leftovers(home: &Home) -> Vec<String> {
    let committed: BTreeSet<String> = history_txs(home).into_iter().collect();
    let orphan = |name: &String| !committed.contains(name.strip_suffix(".wtf").unwrap_or(name));
    let mut out: Vec<String> = names(&home.state().join("journal"))
        .into_iter()
        .map(|n| format!("journal/{n}"))
        .collect();
    out.extend(
        names(&home.state().join("snapshots"))
            .into_iter()
            .filter(orphan)
            .map(|n| format!("<state>/snapshots/{n}")),
    );
    for client in names(&home.root).into_iter().filter(|n| n.starts_with('_')) {
        let meta = home.client(&client).join("Interface/.omaddons");
        out.extend(
            names(&meta.join("stage"))
                .into_iter()
                .map(|n| format!("{client}/stage/{n}")),
        );
        out.extend(
            names(&meta.join("snapshots"))
                .into_iter()
                .filter(orphan)
                .map(|n| format!("{client}/snapshots/{n}")),
        );
    }
    out
}

/// No journal, stage or uncommitted snapshot is left.
fn assert_no_leftovers(log: &TestLog, home: &Home) {
    log.assert_eq(
        "journals, stages and orphan snapshots",
        &leftovers(home),
        &Vec::<String>::new(),
    );
}

/// The committed transaction of a run: `data.tx`, or its first target's.
fn committed_tx(data: &Value) -> String {
    data["tx"]
        .as_str()
        .or_else(|| data["targets"][0]["tx"].as_str())
        .unwrap_or_default()
        .to_owned()
}

/// The `-v` cost notes on a run's stderr.
fn cost_notes(ran: &Ran) -> Vec<String> {
    ran.events()
        .iter()
        .filter(|e| e["event"] == "log")
        .filter_map(|e| e["message"].as_str())
        .filter(|m| m.contains(COST_NOTE))
        .map(str::to_owned)
        .collect()
}

/// A refusal's message names `why` (the under-lock check's reason).
fn assert_names(log: &TestLog, ran: &Ran, why: &str) {
    let message = ran.envelope()["error"]["message"]
        .as_str()
        .unwrap_or_default();
    log.assert_eq(
        &format!("the refusal says: {why}"),
        &message.contains(&format!("(observed changed: {why})")),
        &true,
    );
}

// ---- the worlds ---------------------------------------------------------------------------

/// The engine commands whose witnessed folder the matrix edits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cmd {
    /// `install --file` over Foo v1: Foo replaced, `Foo_Extra` added.
    Install,
    /// `update` to Foo v2 from GitHub: the same operations.
    Update,
    /// `copy` of Foo from retail to Mists Classic: retail's Foo is a copy source.
    Copy,
}

/// A seeded home and the run its scenario binds.
struct World {
    home: Home,
    /// The run without presentation flags, `--dry-run` or `--if-match`.
    args: Vec<String>,
    /// The witnessed folder the hand edits change.
    edit_root: PathBuf,
    /// The client the run changes.
    target: &'static str,
    /// The TOC of a hand-made folder named as one the plan creates.
    appear: PathBuf,
}

impl Cmd {
    /// The seeded world, logged as the step `seed`.
    fn world(self, log: &TestLog) -> World {
        log.step("seed");
        let products: &[&str] = if self == Self::Copy {
            &["wow", "wow_classic"]
        } else {
            &["wow"]
        };
        let home = Home::new(products);
        log.env(&home);
        saved_variables(&home, "_retail_", &["Foo"]);
        let retail = home.addons("_retail_");
        let (args, target, appear) = match self {
            Self::Install => {
                let dl = home.env.root().join("dl");
                let v1 = foo_zip(&dl, "foo-v1.zip", "v1", &[]);
                let v2 = foo_zip(&dl, "foo-v2.zip", "v2", &[EXTRA]);
                ok(
                    log,
                    &home,
                    &[
                        "install",
                        "gh:o/foo",
                        "--file",
                        &v1,
                        "-t",
                        "retail",
                        "--offline",
                    ],
                );
                let args = [
                    "install",
                    "gh:o/foo",
                    "--file",
                    &v2,
                    "-t",
                    "retail",
                    "--offline",
                ];
                (
                    strings(&args),
                    "_retail_",
                    retail.join("Foo_Extra/Foo_Extra.toc"),
                )
            }
            Self::Update => {
                foo_release(&home, "v1", &[]);
                ok(log, &home, &["install", "gh:o/foo", "-t", "retail"]);
                foo_release(&home, "v2", &[EXTRA]);
                let args = ["update", "Foo", "-t", "retail", "--refresh"];
                (
                    strings(&args),
                    "_retail_",
                    retail.join("Foo_Extra/Foo_Extra.toc"),
                )
            }
            Self::Copy => {
                saved_variables(&home, "_classic_", &["Foo"]);
                foo_release(&home, "v1", &[]);
                ok(log, &home, &["install", "gh:o/foo", "-t", "retail"]);
                let args = ["copy", "--from", "retail", "--to", "classic", "Foo"];
                let appear = home.addons("_classic_").join("Foo/Foo.toc");
                (strings(&args), "_classic_", appear)
            }
        };
        World {
            edit_root: retail.join("Foo"),
            home,
            args,
            target,
            appear,
        }
    }
}

impl World {
    /// The preview, logged; returns it and its token.
    fn preview(&self, log: &TestLog) -> (Ran, String) {
        let mut args = self.args.clone();
        args.push("--dry-run".to_owned());
        let ran = run(log, &self.home, &json(&args));
        ran.ok();
        let token = ran.data()["precondition"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        log.assert_eq(
            "a dry run with a token",
            &omaddons::app::precondition::is_token(&token),
            &true,
        );
        log.json("token", &token);
        (ran, token)
    }

    /// The run bound to `token`.
    fn bound(&self, token: &str) -> Vec<String> {
        let mut args = self.args.clone();
        args.extend(strings(&["--if-match", token]));
        json(&args)
    }

    /// A refusal's fix: the preview, presentation flags dropped.
    fn fix(&self) -> Vec<String> {
        let mut fix = strings(&["omaddons"]);
        fix.extend(self.args.iter().cloned());
        fix.push("--dry-run".to_owned());
        fix
    }

    /// The bound run stopped at `point`, `act` done while it waits, then resumed; returns
    /// the finished run.
    fn paused(&self, log: &TestLog, bound: &[String], point: &str, act: impl FnOnce()) -> Ran {
        log.step("bound run, paused");
        let pause = Pause::new(self.home.env.root());
        let child = log
            .cmd(&self.home, bound)
            .failpoints(&format!("{point}=pause"))
            .pause(&pause)
            .spawn();
        log.reached(&pause, point);
        act();
        log.step("resume");
        log.resume(&pause, point);
        let ran = child.wait();
        ran.assert_private();
        ran
    }
}

// ---- the hand edits -----------------------------------------------------------------------

/// What a hand edit does to the witnessed folder (or, for the last, beside it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Edit {
    CleanFileEdited,
    /// Ten files changed before the preview (all a `local_changes_replaced` warning
    /// names), then an eleventh.
    EleventhChangedFile,
    /// Other bytes at the same size, the mtime put back: only the ctime moves.
    SameSizeBytes,
    FileDeleted,
    /// Another file renamed over one: a new inode.
    FileReplaced,
    SymlinkAdded,
    EmptyDirAdded,
    /// A folder named as one the plan creates.
    CreatedNameAppeared,
}

impl Edit {
    /// The folder the edit changes.
    fn root(self, world: &World) -> PathBuf {
        match self {
            Self::CreatedNameAppeared => world.appear.parent().unwrap().to_path_buf(),
            _ => world.edit_root.clone(),
        }
    }

    /// The edit's part made before the preview.
    fn before_preview(self, log: &TestLog, world: &World) {
        if self == Self::EleventhChangedFile {
            for i in 0..10 {
                append(&world.edit_root.join(format!("f{i:02}.lua")), "-- local\n");
            }
            log.note("before the preview: f00.lua … f09.lua each appended `-- local`");
        }
    }

    /// Applies the edit, logged; fails at once when it changed nothing.
    fn apply(self, log: &TestLog, world: &World) {
        let root = self.root(world);
        let before = hash(&root);
        let at = |name: &str| root.join(name);
        let what = match self {
            Self::CleanFileEdited => {
                fs::write(at("f05.lua"), "-- edited by hand, a longer line\n").unwrap();
                "f05.lua rewritten, longer"
            }
            Self::EleventhChangedFile => {
                append(&at("f10.lua"), "-- local\n");
                "f10.lua appended `-- local`"
            }
            Self::SameSizeBytes => {
                let path = at("f06.lua");
                let mtime = fs::metadata(&path).unwrap().modified().unwrap();
                let len = fs::read(&path).unwrap().len();
                let mut other = vec![b'x'; len - 1];
                other.push(b'\n');
                fs::write(&path, other).unwrap();
                let file = fs::File::options().write(true).open(&path).unwrap();
                file.set_times(fs::FileTimes::new().set_modified(mtime))
                    .unwrap();
                "f06.lua's bytes replaced at the same size, its mtime put back"
            }
            Self::FileDeleted => {
                fs::remove_file(at("f07.lua")).unwrap();
                "f07.lua removed"
            }
            Self::FileReplaced => {
                fs::write(at("f08.lua.new"), "-- another file\n").unwrap();
                fs::rename(at("f08.lua.new"), at("f08.lua")).unwrap();
                "f08.lua.new renamed over f08.lua"
            }
            Self::SymlinkAdded => {
                std::os::unix::fs::symlink("f00.lua", at("link.lua")).unwrap();
                "link.lua -> f00.lua added"
            }
            Self::EmptyDirAdded => {
                fs::create_dir(at("empty")).unwrap();
                "empty/ added"
            }
            Self::CreatedNameAppeared => {
                fs::create_dir_all(&root).unwrap();
                fs::write(&world.appear, "## Interface: 120105\n").unwrap();
                "a folder the plan creates made by hand, with a one-line TOC"
            }
        };
        log.note(format!("the edit: {what}"));
        assert!(hash(&root) != before, "the edit changed nothing: {what}");
    }

    /// What the under-lock check says changed.
    fn why(self, world: &World) -> String {
        match self {
            Self::CreatedNameAppeared => {
                let name = world.appear.parent().unwrap().file_name().unwrap();
                format!(
                    "a folder named {} appeared in AddOns",
                    name.to_string_lossy().to_ascii_lowercase()
                )
            }
            _ => "foo changed after the preview".to_owned(),
        }
    }
}

fn append(path: &Path, text: &str) {
    let mut bytes = fs::read(path).unwrap();
    bytes.extend_from_slice(text.as_bytes());
    fs::write(path, bytes).unwrap();
}

/// When the edit lands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum When {
    /// After the preview, before the bound run starts: its own first check (D2, D3).
    Early,
    /// While it waits at `engine.snapshot.prestaged`, after its first check.
    Late,
    /// While it waits at `engine.registered`, holding the lock.
    UnderLock,
}

impl When {
    fn point(self) -> Option<&'static str> {
        match self {
            Self::Early => None,
            Self::Late => Some(PRESTAGED),
            Self::UnderLock => Some(REGISTERED),
        }
    }
}

/// `local_changes_replaced` names exactly f00.lua … f09.lua of Foo.
fn assert_ten_paths(log: &TestLog, preview: &Ran) {
    let messages: Vec<String> = preview.envelope()["warnings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|w| w["code"] == "local_changes_replaced")
        .filter_map(|w| w["message"].as_str().map(str::to_owned))
        .collect();
    log.json("local_changes_replaced", &messages);
    let named: Vec<String> = messages
        .first()
        .and_then(|m| m.split_once(" files (").map(|(_, rest)| rest.to_owned()))
        .and_then(|rest| rest.split_once(')').map(|(list, _)| list.to_owned()))
        .map(|list| list.split(", ").map(str::to_owned).collect())
        .unwrap_or_default();
    let expected: Vec<String> = (0..10).map(|i| format!("Foo/f{i:02}.lua")).collect();
    log.assert_eq(
        "one warning naming exactly the ten paths",
        &(messages.len(), named),
        &(1, expected),
    );
}

/// One row of the witness matrix: the edit at its moment refuses the bound run as
/// `observed`, moves nothing, and the refusal's fix previews a run that commits.
fn row(log: &TestLog, when: When, cmd: Cmd, edit: Edit) {
    let world = cmd.world(log);
    edit.before_preview(log, &world);

    log.step("preview");
    let (preview, token) = world.preview(log);
    if edit == Edit::EleventhChangedFile && cmd != Cmd::Copy {
        assert_ten_paths(log, &preview);
    }
    let bound = world.bound(&token);

    let mut guard = None;
    let mut edited = None;
    let mut edit_now = || {
        log.step("edit");
        edit.apply(log, &world);
        edited = log.tree("the edited folder", &edit.root(&world));
        guard = Some(b::guard(log, &world.home, "after the edit"));
    };
    let ran = match when.point() {
        None => {
            edit_now();
            log.step("bound run");
            run(log, &world.home, &bound)
        }
        Some(point) => world.paused(log, &bound, point, edit_now),
    };

    log.step("refusal");
    b::assert_stale(log, &ran, &["observed"], &world.fix());
    log.json("refusal", &ran.envelope()["error"]);
    if when == When::Early {
        // The first check compares the token's parts, which name no folder.
        assert_names_nothing(log, &ran);
    } else {
        assert_names(log, &ran, &edit.why(&world));
    }

    log.step("nothing moved");
    let guard = guard.unwrap();
    guard.unchanged(log);
    assert_no_leftovers(log, &world.home);

    log.step("the fix");
    let fix: Vec<String> = world.fix()[1..].to_vec();
    let previewed = run(log, &world.home, &json(&fix));
    if edit == Edit::CreatedNameAppeared {
        log.note("the hand-made folder is planned around as planning decides; the preview changes nothing");
        guard.unchanged(log);
        return;
    }
    let fresh = previewed.ok().data()["precondition"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let committed = run(log, &world.home, &world.bound(&fresh));
    committed.ok();
    if cmd == Cmd::Copy {
        log.assert_eq(
            "the copy source is as the edit left it",
            &hash(&world.edit_root),
            &edited,
        );
    } else {
        let tx = committed_tx(committed.data());
        let kept = world
            .home
            .client(world.target)
            .join("Interface/.omaddons/snapshots")
            .join(&tx)
            .join("AddOns/Foo");
        log.assert_eq(
            "the snapshot keeps the edited folder",
            &hash(&kept),
            &edited,
        );
    }
}

/// A first-check refusal: the token's parts, with no reason.
fn assert_names_nothing(log: &TestLog, ran: &Ran) {
    let message = ran.envelope()["error"]["message"]
        .as_str()
        .unwrap_or_default();
    log.assert_eq(
        "the refusal names the parts only",
        &message.contains("(observed changed);"),
        &true,
    );
}

macro_rules! witness_rows {
    ($($name:ident: $when:ident, $cmd:ident, $edit:ident;)*) => {$(
        #[test]
        fn $name() {
            let log = crate::common::tlog::tlog!();
            row(&log, When::$when, Cmd::$cmd, Edit::$edit);
        }
    )*};
}

witness_rows! {
    witness_late_install_clean_file_edited: Late, Install, CleanFileEdited;
    witness_late_install_eleventh_changed_file: Late, Install, EleventhChangedFile;
    witness_late_install_same_size_edit: Late, Install, SameSizeBytes;
    witness_late_install_file_deleted: Late, Install, FileDeleted;
    witness_late_install_file_replaced: Late, Install, FileReplaced;
    witness_late_install_symlink_added: Late, Install, SymlinkAdded;
    witness_late_install_empty_dir_added: Late, Install, EmptyDirAdded;
    witness_late_install_created_name_appeared: Late, Install, CreatedNameAppeared;
    witness_late_update_clean_file_edited: Late, Update, CleanFileEdited;
    witness_late_update_eleventh_changed_file: Late, Update, EleventhChangedFile;
    witness_late_update_same_size_edit: Late, Update, SameSizeBytes;
    witness_late_update_file_deleted: Late, Update, FileDeleted;
    witness_late_update_file_replaced: Late, Update, FileReplaced;
    witness_late_update_symlink_added: Late, Update, SymlinkAdded;
    witness_late_update_empty_dir_added: Late, Update, EmptyDirAdded;
    witness_late_update_created_name_appeared: Late, Update, CreatedNameAppeared;
    witness_late_copy_clean_file_edited: Late, Copy, CleanFileEdited;
    witness_late_copy_eleventh_changed_file: Late, Copy, EleventhChangedFile;
    witness_late_copy_same_size_edit: Late, Copy, SameSizeBytes;
    witness_late_copy_file_deleted: Late, Copy, FileDeleted;
    witness_late_copy_file_replaced: Late, Copy, FileReplaced;
    witness_late_copy_symlink_added: Late, Copy, SymlinkAdded;
    witness_late_copy_empty_dir_added: Late, Copy, EmptyDirAdded;
    witness_late_copy_created_name_appeared: Late, Copy, CreatedNameAppeared;
    witness_early_install_clean_file_edited: Early, Install, CleanFileEdited;
    witness_early_update_clean_file_edited: Early, Update, CleanFileEdited;
    witness_early_copy_clean_file_edited: Early, Copy, CleanFileEdited;
    witness_under_lock_update_clean_file_edited: UnderLock, Update, CleanFileEdited;
}

// ---- operations re-judged under the lock, and the dependencies ------------------------------

/// A forget (every folder of Foo gone) whose folder is put back by hand after the bound
/// run's own check: the forget no longer applies, so the operations re-judged under the
/// lock differ from the approved ones (`selection`). A forget moves no folder, so the
/// witness has nothing to refuse first.
#[test]
fn witness_rejudged_sync_prune_forget_is_selection() {
    let log = crate::common::tlog::tlog!();
    log.step("seed: Foo (two folders) and Bar installed, Foo's folders deleted by hand");
    let home = Home::new(&["wow"]);
    log.env(&home);
    saved_variables(&home, "_retail_", &["Foo", "Bar"]);
    publish(&home, "foo", "Foo", "v1", &["FooOld"]);
    publish(&home, "bar", "Bar", "v1", &[]);
    ok(
        &log,
        &home,
        &["install", "gh:o/foo", "gh:o/bar", "-t", "retail"],
    );
    let addons = home.addons("_retail_");
    fs::remove_dir_all(addons.join("Foo")).unwrap();
    fs::remove_dir_all(addons.join("FooOld")).unwrap();
    home.write_desired("schema_version = \"1.0\"\n\n[family.mainline]\naddons = [\"Bar\"]\n");
    let args = strings(&["sync", "--prune", "-t", "retail"]);
    let world = World {
        home,
        args,
        edit_root: addons.join("Foo"),
        target: "_retail_",
        appear: addons.join("Foo/Foo.toc"),
    };

    log.step("preview: Foo forgotten");
    let (preview, token) = world.preview(&log);
    log.json("items", &b::items(preview.data()));

    let mut guard = None;
    let ran = world.paused(&log, &world.bound(&token), PRESTAGED, || {
        log.step("Foo/Foo.toc put back by hand");
        fs::create_dir_all(&world.edit_root).unwrap();
        fs::write(&world.appear, "## Interface: 120105\n").unwrap();
        guard = Some(b::guard(&log, &world.home, "after the edit"));
    });

    log.step("refusal");
    b::assert_stale(&log, &ran, &["selection"], &world.fix());
    log.step("nothing moved: Foo still recorded");
    guard.unwrap().unchanged(&log);
    assert_no_leftovers(&log, &world.home);
    log.assert_eq(
        "Foo's record",
        &state::record(&world.home, "retail")
            .addons
            .contains_key("foo"),
        &true,
    );
}

/// Little's update brings Big, which the plan installs; Big made by hand after the bound
/// run's own check is a name the plan creates, so the witness refuses it (`observed`)
/// before the operations are re-judged.
#[test]
fn witness_rejudged_dependency_satisfied_by_hand_is_observed() {
    let log = crate::common::tlog::tlog!();
    let world = little(&log, "Big");
    catalog(
        &world.home,
        &[("Little", "little", &["Little"]), ("Big", "big", &["Big"])],
    );
    release(&world.home, "big", "v1", &[folder("Big", &[])]);

    log.step("preview: Little updated, Big installed with it");
    let (preview, token) = world.preview(&log);
    log.assert_eq(
        "the preview's items",
        &item_names(preview.data()),
        &strings(&["Little", "Big"]),
    );

    let mut guard = None;
    let ran = world.paused(&log, &world.bound(&token), PRESTAGED, || {
        log.step("Big made by hand");
        let big = world.home.addons("_retail_").join("Big");
        fs::create_dir_all(&big).unwrap();
        fs::write(big.join("Big.toc"), "## Interface: 120105\n").unwrap();
        guard = Some(b::guard(&log, &world.home, "after the edit"));
    });

    log.step("refusal");
    b::assert_stale(&log, &ran, &["observed"], &world.fix());
    assert_names(&log, &ran, "a folder named big appeared in AddOns");
    guard.unwrap().unchanged(&log);
    assert_no_leftovers(&log, &world.home);
}

/// Little's update requires Lib, an unmanaged folder the preview relies on and installs
/// nothing for; Lib moved out of AddOns after the bound run's own check refuses it.
#[test]
fn witness_unmanaged_dependency_gone_is_observed() {
    let log = crate::common::tlog::tlog!();
    let world = little(&log, "Lib");
    let lib = world.home.addons("_retail_");
    addons::addon_folder(&lib, "Lib", &[("Lib.toc", "## Interface: 120105\n")], &[]);

    log.step("preview: Little updated; Lib is there, unmanaged, and nothing installs");
    let (preview, token) = world.preview(&log);
    log.assert_eq(
        "the preview's items",
        &item_names(preview.data()),
        &strings(&["Little"]),
    );

    let mut guard = None;
    let ran = world.paused(&log, &world.bound(&token), PRESTAGED, || {
        log.step("Lib moved out of AddOns");
        let lib = world.home.addons("_retail_").join("Lib");
        fs::rename(&lib, world.home.env.root().join("Lib-moved")).unwrap();
        guard = Some(b::guard(&log, &world.home, "after the edit"));
    });

    log.step("refusal");
    b::assert_stale(&log, &ran, &["observed"], &world.fix());
    assert_names(
        &log,
        &ran,
        "lib, which Little requires, is no longer in AddOns",
    );
    guard.unwrap().unchanged(&log);
    assert_no_leftovers(&log, &world.home);
}

/// Release `tag` of `gh:o/<repo>`, shipping `folders` (each its TOC only), as its latest.
fn release(home: &Home, repo: &str, tag: &str, folders: &[Folder<'_>]) {
    let base = format!("https://github.com/o/{repo}");
    release_graph(
        &home.http,
        &[Node {
            base: &base,
            tag,
            folders,
        }],
    );
}

/// `<config>/omaddons/catalog.d/50-witness.toml`: one entry per (name, repository under
/// `o/`, folders).
fn catalog(home: &Home, entries: &[(&str, &str, &[&str])]) {
    let mut text = String::from("schema_version = \"1.0\"\n");
    for (name, repo, folders) in entries {
        let folders: Vec<String> = folders.iter().map(|f| format!("{f:?}")).collect();
        text.push_str(&format!(
            "\n[[addon]]\nname = {name:?}\nref = \"gh:o/{repo}\"\nfolders = [{}]\n",
            folders.join(", ")
        ));
    }
    let dir = home.env.config().join("omaddons/catalog.d");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("50-witness.toml"), text).unwrap();
}

/// Little v1 installed and v2, requiring `requires`, published: the world of
/// `update Little -t retail --refresh`.
fn little(log: &TestLog, requires: &str) -> World {
    log.step("seed: Little v1 installed, v2 requires a folder");
    let home = Home::new(&["wow"]);
    log.env(&home);
    saved_variables(&home, "_retail_", &["Little"]);
    release(&home, "little", "v1", &[folder("Little", &[])]);
    ok(log, &home, &["install", "gh:o/little", "-t", "retail"]);
    let deps = [requires];
    release(&home, "little", "v2", &[folder("Little", &deps)]);
    let addons = home.addons("_retail_");
    World {
        home,
        args: strings(&["update", "Little", "-t", "retail", "--refresh"]),
        edit_root: addons.join("Little"),
        target: "_retail_",
        appear: addons.join(format!("{requires}/{requires}.toc")),
    }
}

/// The names of a preview's items, in order.
fn item_names(data: &Value) -> Vec<String> {
    b::items(data)
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|i| i["name"].as_str().map(str::to_owned))
        .collect()
}

// ---- an unchanged world commits exactly its preview ---------------------------------------

/// The binding commands an unchanged world must commit exactly as previewed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Same {
    Install,
    Update,
    Copy,
    /// A pinned entry at a newer version: Foo replaced, pinned.
    Sync,
    Pin,
    Unpin,
    /// An unmanaged Bar, named in the catalog, adopted by name.
    Adopt,
}

impl Same {
    fn world(self, log: &TestLog) -> World {
        match self {
            Self::Install => Cmd::Install.world(log),
            Self::Update => Cmd::Update.world(log),
            Self::Copy => Cmd::Copy.world(log),
            Self::Sync => {
                let world = Cmd::Update.world(log);
                world.home.write_desired(
                    "schema_version = \"1.0\"\n\n[[family.mainline.addon]]\nref = \"gh:o/foo\"\nversion = \"v2\"\npinned = true\n",
                );
                World {
                    args: strings(&["sync", "-t", "retail"]),
                    ..world
                }
            }
            Self::Pin => World {
                args: strings(&["pin", "Foo", "-t", "retail"]),
                ..Cmd::Update.world(log)
            },
            Self::Unpin => {
                let world = Cmd::Update.world(log);
                ok(log, &world.home, &["pin", "Foo", "-t", "retail"]);
                World {
                    args: strings(&["unpin", "Foo", "-t", "retail"]),
                    ..world
                }
            }
            Self::Adopt => {
                log.step("seed: Bar unpacked by hand, named in the catalog");
                let home = Home::new(&["wow"]);
                log.env(&home);
                saved_variables(&home, "_retail_", &["Bar"]);
                let files = entries("Bar", "v1", &[]);
                let borrowed: Vec<(&str, &str)> = files
                    .iter()
                    .map(|(p, t)| (p.as_str(), t.as_str()))
                    .collect();
                let retail = home.addons("_retail_");
                addons::unpack(&retail, &borrowed);
                catalog(&home, &[("Bar", "bar", &["Bar"])]);
                World {
                    home,
                    args: strings(&["adopt", "Bar", "-t", "retail", "--no-verify"]),
                    edit_root: retail.join("Bar"),
                    target: "_retail_",
                    appear: retail.join("Bar/Bar.toc"),
                }
            }
        }
    }
}

/// Every item of a run's data: `data.items`, or each target's.
fn all_items(data: &Value) -> Vec<Value> {
    match data["items"].as_array() {
        Some(items) => items.clone(),
        None => data["targets"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|t| t["items"].as_array().cloned().unwrap_or_default())
            .collect(),
    }
}

/// The folder operations a preview's items list, as history writes them, sorted.
fn preview_ops(data: &Value) -> Vec<(String, String)> {
    let mut ops: Vec<(String, String)> = all_items(data)
        .iter()
        .flat_map(|item| {
            [
                ("added", "add"),
                ("replaced", "replace"),
                ("removed", "remove"),
                ("recorded", "record"),
            ]
            .into_iter()
            .flat_map(move |(list, op)| {
                item["folders"][list]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|f| f.as_str().map(|f| (op.to_owned(), f.to_owned())))
                    .collect::<Vec<_>>()
            })
        })
        .collect();
    ops.sort();
    ops
}

/// A history line's AddOns folder operations, sorted.
fn line_ops(line: &Value) -> Vec<(String, String)> {
    let mut ops: Vec<(String, String)> = line["ops"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|o| {
            matches!(
                o["op"].as_str(),
                Some("add" | "replace" | "remove" | "record")
            )
        })
        .map(|o| {
            (
                o["op"].as_str().unwrap_or_default().to_owned(),
                o["folder"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect();
    ops.sort();
    ops
}

fn history_lines(home: &Home) -> Vec<Value> {
    fs::read_to_string(home.state().join("history.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .collect()
}

/// `id`'s record without what differs between two homes running one change: the
/// target's key, and each addon's `tx_id` and `installed_at`.
fn record_view(home: &Home, id: &str) -> Value {
    let mut record = serde_json::to_value(state::record(home, id)).unwrap();
    if let Some(fields) = record.as_object_mut() {
        fields.remove("key");
    }
    if let Some(addons) = record["addons"].as_object_mut() {
        for addon in addons.values_mut() {
            if let Some(fields) = addon.as_object_mut() {
                fields.remove("tx_id");
                fields.remove("installed_at");
            }
        }
    }
    record
}

/// Preview, then the bound run with nothing changed between: it commits what the preview
/// listed, and what a twin home's unbound run commits.
fn unchanged_world(log: &TestLog, same: Same) {
    let world = same.world(log);
    log.step("preview");
    let (preview, token) = world.preview(log);
    let approved = preview_ops(preview.data());
    log.json("the preview's folder operations", &approved);
    log.assert_eq(
        "the preview changes something",
        &!approved.is_empty(),
        &true,
    );
    let before = history_lines(&world.home).len();

    log.step("bound run, nothing changed since the preview");
    let mut bound = world.bound(&token);
    bound.push("-v".to_owned());
    let ran = run(log, &world.home, &bound);
    ran.ok();
    let notes = cost_notes(&ran);
    log.json("cost notes", &notes);
    log.assert_eq(
        "one cost note, for the one target committed",
        &notes.len(),
        &1,
    );
    let lines = history_lines(&world.home);
    log.assert_eq("one new history line", &lines.len(), &(before + 1));
    log.assert_eq(
        "its folder operations are the preview's",
        &line_ops(&lines[before]),
        &approved,
    );

    log.step("the twin: the same seed, the same command unbound");
    let twin = same.world(log);
    run(log, &twin.home, &json(&twin.args)).ok();
    log.step("the bound run left what the twin's unbound run left");
    let id = world.target.trim_matches('_');
    log.assert_eq(
        "AddOns as the twin's unbound run leaves it",
        &hash(&world.home.addons(world.target)),
        &hash(&twin.home.addons(twin.target)),
    );
    log.assert_eq(
        "the record as the twin's unbound run leaves it",
        &record_view(&world.home, id),
        &record_view(&twin.home, id),
    );
}

macro_rules! unchanged_rows {
    ($($name:ident: $same:ident;)*) => {$(
        #[test]
        fn $name() {
            let log = crate::common::tlog::tlog!();
            unchanged_world(&log, Same::$same);
        }
    )*};
}

unchanged_rows! {
    witness_unchanged_world_commits_exactly_the_preview_install: Install;
    witness_unchanged_world_commits_exactly_the_preview_update: Update;
    witness_unchanged_world_commits_exactly_the_preview_copy: Copy;
    witness_unchanged_world_commits_exactly_the_preview_sync: Sync;
    witness_unchanged_world_commits_exactly_the_preview_pin: Pin;
    witness_unchanged_world_commits_exactly_the_preview_unpin: Unpin;
    witness_unchanged_world_commits_exactly_the_preview_adopt: Adopt;
}

/// A file touched (its mtime moved, its bytes kept) after the bound run's own check: the
/// under-lock check hashes the folder again, finds the same content, and commits.
#[test]
fn witness_late_touch_still_commits() {
    let log = crate::common::tlog::tlog!();
    let world = Cmd::Update.world(&log);
    log.step("preview");
    let (_, token) = world.preview(&log);
    let mut bound = world.bound(&token);
    bound.push("-v".to_owned());
    let ran = world.paused(&log, &bound, PRESTAGED, || {
        log.step("f03.lua touched, its bytes kept");
        let file = fs::File::options()
            .write(true)
            .open(world.edit_root.join("f03.lua"))
            .unwrap();
        file.set_modified(std::time::SystemTime::now()).unwrap();
    });
    log.step("committed");
    ran.ok();
    let notes = cost_notes(&ran);
    log.json("cost notes", &notes);
    log.assert_eq(
        "one note: Foo hashed again (rehashed 1), and it passed",
        &(
            notes.len(),
            notes.iter().all(|n| n.contains(", rehashed 1,")),
        ),
        &(1, true),
    );
}

// ---- the guarantee's scope ----------------------------------------------------------------

const EDITED: &str = "-- edited after the check\n";

/// An edit inside a folder after both under-lock checks keeps the folder's identity, so
/// it is outside the guarantee by design (M4-D60): the run commits, the transaction's
/// snapshot keeps the edited folder, and `undo` brings it back byte for byte.
#[test]
fn witness_scope_an_edit_after_the_check_is_committed_and_undoable() {
    let log = crate::common::tlog::tlog!();
    let world = Cmd::Update.world(&log);
    log.step("preview");
    let (_, token) = world.preview(&log);
    let ran = world.paused(&log, &world.bound(&token), REVALIDATED, || {
        log.step("f04.lua edited in place, after both checks");
        fs::write(world.edit_root.join("f04.lua"), EDITED).unwrap();
        log.tree("edited Foo", &world.edit_root);
    });
    log.step("committed");
    ran.ok();
    let tx = committed_tx(ran.data());
    let kept = world
        .home
        .client("_retail_")
        .join("Interface/.omaddons/snapshots")
        .join(&tx)
        .join("AddOns/Foo/f04.lua");
    log.assert_eq(
        "the snapshot keeps the edit",
        &fs::read_to_string(kept).unwrap_or_default(),
        &EDITED.to_owned(),
    );

    log.step("undo");
    ok(&log, &world.home, &["undo", &tx]);
    log.assert_unchanged("edited Foo", &world.edit_root);
    log.assert_eq(
        "Foo_Extra, which the update added, is gone",
        &world.appear.parent().unwrap().exists(),
        &false,
    );
}

/// Alpha and Beta at v1, v2 published for each, 12 files apiece: one `update` that
/// exchanges two folders, Alpha's first (the swap renames by folder key).
fn alpha_beta(log: &TestLog) -> World {
    log.step("seed: Alpha and Beta at v1, v2 published");
    let home = Home::new(&["wow"]);
    log.env(&home);
    saved_variables(&home, "_retail_", &["Alpha", "Beta"]);
    for (repo, name) in [("alpha", "Alpha"), ("beta", "Beta")] {
        publish(&home, repo, name, "v1", &[]);
    }
    ok(
        log,
        &home,
        &["install", "gh:o/alpha", "gh:o/beta", "-t", "retail"],
    );
    for (repo, name) in [("alpha", "Alpha"), ("beta", "Beta")] {
        publish(&home, repo, name, "v2", &[]);
    }
    let addons = home.addons("_retail_");
    World {
        home,
        args: strings(&["update", "-t", "retail", "--refresh"]),
        edit_root: addons.join("Beta"),
        target: "_retail_",
        appear: addons.join("Beta/Beta.toc"),
    }
}

/// Beta moved aside and replaced by an exact copy (the same names, bytes, modes and
/// mtimes; another identity) while the run waits after both under-lock checks, then the
/// run resumes. The swap exchanges Alpha first (it renames by folder key), so Beta's
/// journaled identity check refuses with an exchange already made, which the rollback
/// undoes. What the run must leave alone is recorded before it starts: the copy hashes as
/// Beta did. Returns the run.
fn replaced_at_the_last_moment(log: &TestLog, world: &World, run: &[String]) -> Ran {
    let generation = state::record(&world.home, "retail").generation;
    let guard = b::guard(log, &world.home, "before the run");
    let ran = world.paused(log, run, REVALIDATED, || {
        log.step("Beta replaced by an exact copy: another identity");
        let aside = world.home.env.root().join("Beta-aside");
        fs::rename(&world.edit_root, &aside).unwrap();
        copy_tree_exact(&aside, &world.edit_root);
    });
    log.step("rolled back: AddOns, records and history as before the run");
    guard.unchanged(log);
    assert_no_leftovers(log, &world.home);
    log.assert_eq(
        "the generation",
        &state::record(&world.home, "retail").generation,
        &generation,
    );
    ran
}

/// A folder replaced after both under-lock checks fails its rename's identity predicate:
/// the bound run is refused as `observed`, not retryable, and rolled back.
#[test]
fn predicate_a_folder_replaced_at_the_last_moment_rolls_back() {
    let log = crate::common::tlog::tlog!();
    let world = alpha_beta(&log);
    log.step("preview");
    let (_, token) = world.preview(&log);
    let ran = replaced_at_the_last_moment(&log, &world, &world.bound(&token));
    b::assert_stale(&log, &ran, &["observed"], &world.fix());
}

/// Unbound, the same refusal is the retryable `plan_stale`, whose fix runs the command
/// again (the output boundary's rerun of every retryable `plan_stale`).
#[test]
fn predicate_unbound_folder_replaced_at_the_last_moment_rolls_back_retryable() {
    let log = crate::common::tlog::tlog!();
    let world = alpha_beta(&log);
    let ran = replaced_at_the_last_moment(&log, &world, &json(&world.args));
    log.step("refusal");
    let error = &ran.fails(1, "plan_stale").envelope()["error"];
    let mut rerun = strings(&["omaddons"]);
    rerun.extend(world.args.iter().cloned());
    log.assert_eq(
        "(retryable, fix_argv: the same command again)",
        &(&error["retryable"], &error["fix_argv"]),
        &(&json!(true), &json!(rerun)),
    );
}
