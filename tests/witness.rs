//! e2e suite: the approval witness's adversarial matrix (M4-D60): D1's design gate.
//!
//! Each row plans on a fresh home with witnesses on and keeps the token; changes the home
//! in one adversarial way; plans again; and checks what the second token says. Edits
//! that try to slip past the witness move `observed` (same-size edits, a restored mtime,
//! the eleventh file a warning never names, symlinks and empty directories). Changes
//! that are no change leave the token equal (a `touch`, a `chmod`, another process's
//! clock and transaction). Entries the witness must never open or follow (a FIFO, a
//! symlink to `/`, a loop) finish in time. Unreadable or oversized folders fail their
//! target alone. D8 proves the under-lock half with the same witness; D2 and D3 the
//! command-level refusals.

// The rows' homes and edits are test setup: a failed write is the test's failure.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs;
use std::io::Write as _;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};

use common::http::gh_release;
use common::planning::{ManagedAddon, PlanOpts, PlanningHome};
use common::tlog::{TestLog, tlog};
use omaddons::app::precondition::{Part, PlanExtras, Precondition, TargetRef, check};
use omaddons::engine::edit::{AdoptSpec, RecordEdit};
use omaddons::engine::inspect::ArchiveLimits;
use omaddons::engine::plan::{AnyFlavorIntent, PinIntent, Plan, RequestedChange, TargetRequest};
use omaddons::engine::witness::{CopyFrom, WitnessScope};
use omaddons::error::ErrorCode;
use omaddons::model::{
    AddonReason, AddonRecord, AddonState, Channel, FolderName, Target, TxId, TxKind,
    VersionConfidence,
};
use omaddons::state::files_index::index_live_folder;
use serde_json::{Value, json};

const TOC: &str = "## Interface: 120105\n";
/// Foo's eleven files (86 bytes in all).
const FOO_FILES: [(&str, &str); 11] = [
    ("Foo.toc", TOC),
    ("Foo.lua", "-- foo v1\n"),
    ("libs/lib.lua", "-- lib\n"),
    ("f01.lua", "-- 01\n"),
    ("f02.lua", "-- 02\n"),
    ("f03.lua", "-- 03\n"),
    ("f04.lua", "-- 04\n"),
    ("f05.lua", "-- 05\n"),
    ("f06.lua", "-- 06\n"),
    ("f07.lua", "-- 07\n"),
    ("f08.lua", "-- 08\n"),
];
/// Bar's two files (32 bytes in all).
const BAR_FILES: [(&str, &str); 2] = [("Bar.toc", TOC), ("Bar.lua", "-- mine 32\n")];
/// How long one plan may take before the row counts as hung.
const LIMIT: Duration = Duration::from_secs(10);

// ---------------------------------------------------------------------------------------
// Homes.

/// A home, what its plans request, and how they plan.
struct Case {
    home: PlanningHome,
    retail: Target,
    other: Target,
    requests: fn(&Case) -> Vec<TargetRequest>,
    opts: PlanOpts,
}

impl Case {
    fn new() -> Self {
        let home = PlanningHome::new();
        let (retail, other) = (home.retail.clone(), home.other.clone());
        Self {
            home,
            retail,
            other,
            requests: |_| Vec::new(),
            opts: PlanOpts::default(),
        }
    }

    fn addons(&self) -> PathBuf {
        self.retail.addons_dir()
    }

    fn plan(&self) -> (Plan, Precondition) {
        let plan = self.home.plan((self.requests)(self), &self.opts);
        let token = self.home.token("install", &plan, &PlanExtras::default());
        (plan, token)
    }

    /// Serves `gh:o/<name>` at `tag` with one retail zip of `files` under `folder`.
    fn release(&self, name: &str, tag: &str, folder: &str, files: &[(&str, &str)]) {
        let entries: Vec<(String, &str)> = files
            .iter()
            .map(|(path, text)| (format!("{folder}/{path}"), *text))
            .collect();
        gh_release(
            &self.home.scenario,
            &format!("https://github.com/o/{name}"),
            tag,
            &[(
                &format!("{folder}.zip"),
                &[("mainline", 120_105)],
                entries.iter().map(|(p, t)| (p.as_str(), *t)).collect(),
            )],
        );
    }
}

fn install(r: &str) -> RequestedChange {
    RequestedChange::Install {
        r#ref: r.parse().unwrap(),
        channel: Channel::Stable,
        channel_explicit: false,
        version: None,
        file: None,
        name: None,
        pin: PinIntent::Keep,
        reason: AddonReason::Explicit,
        reinstall: false,
        blob_sha256: None,
        asset: None,
        revision: None,
        any_flavor: AnyFlavorIntent::Keep,
    }
}

fn update(addon: &str) -> RequestedChange {
    RequestedChange::Update {
        addon: addon.into(),
        installed_primary_interface: None,
    }
}

fn request(target: &Target, changes: Vec<RequestedChange>, kind: TxKind) -> TargetRequest {
    TargetRequest {
        target: target.clone(),
        changes,
        default_kind: kind,
        dependency_locks: BTreeMap::new(),
    }
}

/// The adoption of the hand-made `folder`, indexed as it is now.
fn adoption(case: &Case, folder: &str) -> RequestedChange {
    let name = FolderName::new(folder).unwrap();
    let index = index_live_folder(&case.addons().join(folder), &ArchiveLimits::default()).unwrap();
    let record = AddonRecord {
        r#ref: format!("gh:o/{}", folder.to_ascii_lowercase())
            .parse()
            .unwrap(),
        name: folder.into(),
        version: "unknown".into(),
        version_confidence: VersionConfidence::Toc,
        release_id: None,
        channel: Channel::Stable,
        pinned: false,
        reason: AddonReason::Explicit,
        required_by: Vec::new(),
        folders: vec![name.clone()],
        state: AddonState::Ok,
        installed_at: "2026-10-01T00:00:00Z".parse().unwrap(),
        tx_id: TxId::new(),
        blob_sha256: None,
        revision: None,
        left_out: Vec::new(),
        any_flavor: false,
        skipped_releases: Vec::new(),
    };
    RequestedChange::Edit {
        addon: folder.into(),
        edit: RecordEdit::Adopt(Box::new(AdoptSpec {
            record,
            folders: vec![(name, index)],
        })),
    }
}

/// Retail manages Foo v1 (eleven files, indexed past the racy slack, so a witness reuses
/// them); the request updates it to v2, so Foo is replaced.
fn managed() -> Case {
    let mut case = Case::new();
    let mut retail = case.retail.clone();
    case.home.manage(
        &mut retail,
        &[ManagedAddon {
            name: "Foo",
            r#ref: "gh:o/foo",
            release: "v1",
            version: "1",
            folders: &[("Foo", &FOO_FILES[..])],
        }],
    );
    case.retail = retail;
    case.release(
        "foo",
        "v2",
        "Foo",
        &[("Foo.toc", TOC), ("Foo.lua", "-- foo v2\n")],
    );
    case.requests = |c| vec![request(&c.retail, vec![update("foo")], TxKind::Update)];
    case
}

/// Retail has a hand-made Bar, which `install gh:o/bar --force` replaces.
fn unmanaged() -> Case {
    let mut case = Case::new();
    case.home.hand_made(&case.retail, "Bar", &BAR_FILES);
    case.release("bar", "v1", "Bar", &[("Bar.toc", TOC)]);
    case.opts.force = true;
    case.requests = |c| {
        vec![request(
            &c.retail,
            vec![install("gh:o/bar")],
            TxKind::Install,
        )]
    };
    case
}

/// A first install into a client without `Interface/AddOns`.
fn fresh() -> Case {
    let mut case = Case::new();
    fs::remove_dir_all(case.addons()).unwrap();
    case.release("foo", "v1", "Foo", &[("Foo.toc", TOC)]);
    case.requests = |c| {
        vec![request(
            &c.retail,
            vec![install("gh:o/foo")],
            TxKind::Install,
        )]
    };
    case
}

/// Retail installs Foo while the witness covers the 3.3.5a client's Foo as copy's source.
fn copy() -> Case {
    let mut case = Case::new();
    case.home.hand_made(
        &case.other,
        "Foo",
        &[
            ("Foo.toc", "## Interface: 30300\n"),
            ("Foo.lua", "-- source\n"),
        ],
    );
    case.release("foo", "v1", "Foo", &[("Foo.toc", TOC)]);
    case.opts.scope = WitnessScope {
        copy_from: Some(CopyFrom {
            target: case.other.clone(),
            folders: vec![FolderName::new("Foo").unwrap()],
        }),
    };
    case.requests = |c| {
        vec![request(
            &c.retail,
            vec![install("gh:o/foo")],
            TxKind::Install,
        )]
    };
    case
}

/// Retail, registered, adopts its hand-made Hand.
fn adopt() -> Case {
    let mut case = Case::new();
    let mut retail = case.retail.clone();
    case.home.manage(&mut retail, &[]);
    case.retail = retail;
    case.home.hand_made(
        &case.retail,
        "Hand",
        &[("Hand.toc", TOC), ("Hand.lua", "-- hand\n")],
    );
    case.requests = |c| vec![request(&c.retail, vec![adoption(c, "Hand")], TxKind::Adopt)];
    case
}

// ---------------------------------------------------------------------------------------
// Edits.

fn foo(case: &Case, rel: &str) -> PathBuf {
    case.addons().join("Foo").join(rel)
}

fn bar(case: &Case, rel: &str) -> PathBuf {
    case.addons().join("Bar").join(rel)
}

/// Flips the first byte of `path`, keeping its size.
fn flip(path: &Path) {
    let mut bytes = fs::read(path).unwrap();
    bytes[0] ^= 0x01;
    fs::write(path, bytes).unwrap();
}

/// Sets `path`'s mtime.
fn set_mtime(path: &Path, at: SystemTime) {
    fs::File::options()
        .append(true)
        .open(path)
        .unwrap()
        .set_modified(at)
        .unwrap();
}

/// Every file under `dir`.
fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if fs::symlink_metadata(&path).unwrap().is_dir() {
            out.extend(files_under(&path));
        } else {
            out.push(path);
        }
    }
    out
}

fn mkfifo(path: &Path) {
    rustix::fs::mknodat(
        rustix::fs::CWD,
        path,
        rustix::fs::FileType::Fifo,
        rustix::fs::Mode::from_raw_mode(0o644),
        0,
    )
    .unwrap();
}

// ---------------------------------------------------------------------------------------
// Running a row.

/// What the second token says against the first.
enum Expect {
    /// `plan_stale` naming exactly these parts.
    Moves(&'static [Part]),
    Equal,
    /// The first target has no work and every change of it failed with `code` and
    /// `details.reason`; the other targets plan and witness normally.
    Fails {
        code: ErrorCode,
        reason: &'static str,
    },
}

/// One adversarial case: its home, the change between the two plans (described), and
/// what the second token must say. `then` checks what is particular to the row.
struct Row {
    home: fn() -> Case,
    what: &'static str,
    mutate: fn(&mut Case),
    expect: Expect,
    then: fn(&TestLog, &Case, &Plan, &Plan),
}

fn row(home: fn() -> Case, what: &'static str, mutate: fn(&mut Case), expect: Expect) -> Row {
    Row {
        home,
        what,
        mutate,
        expect,
        then: |_, _, _, _| {},
    }
}

/// Runs `f`, aborting the whole run when it has not returned within [`LIMIT`]: a FIFO
/// opened or a link followed would otherwise hang the suite.
fn within<T>(row: &str, f: impl FnOnce() -> T) -> T {
    let done = Arc::new(AtomicBool::new(false));
    let watch = Arc::clone(&done);
    let row = row.to_owned();
    std::thread::spawn(move || {
        let start = Instant::now();
        while start.elapsed() < LIMIT {
            if watch.load(Ordering::SeqCst) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = writeln!(
            std::io::stderr(),
            "{row}: a plan did not finish within {LIMIT:?}: the witness waited on an entry"
        );
        std::process::abort();
    });
    let out = f();
    done.store(true, Ordering::SeqCst);
    out
}

/// Each witness's public parts and cost.
fn witnesses(plan: &Plan) -> Value {
    json!(
        plan.witnesses
            .iter()
            .map(|w| {
                let cost = w.cost();
                json!({
                    "target": w.target.to_string(),
                    "marker": w.marker,
                    "folders": w.folders.iter().map(|f| json!({
                        "role": f.role.wire(), "key": f.key, "digest": f.digest.to_string(),
                    })).collect::<Vec<_>>(),
                    "absent": w.absent,
                    "inputs": w.inputs.iter().map(|i| json!({
                        "kind": i.kind.wire(), "source": i.source, "digest": i.digest,
                    })).collect::<Vec<_>>(),
                    "dependencies": w.dependencies.iter().map(|d| json!({
                        "requester": d.requester, "folder": d.folder,
                        "by": d.by.wire(), "detail": d.by.detail(),
                    })).collect::<Vec<_>>(),
                    "cost": {
                        "files_hashed": cost.files_hashed,
                        "files_reused": cost.files_reused,
                        "bytes_hashed": cost.bytes_hashed,
                    },
                })
            })
            .collect::<Vec<_>>()
    )
}

fn failures(plan: &Plan) -> Value {
    json!(
        plan.failures
            .iter()
            .map(|f| json!({
                "target": f.target, "addon": f.addon, "code": f.error.code.wire(),
                "message": f.error.message, "details": f.error.details,
            }))
            .collect::<Vec<_>>()
    )
}

fn logged(log: &TestLog, plan: &Plan, token: &Precondition) {
    log.json("parts", &token.parts());
    log.json("witness", &witnesses(plan));
    if !plan.failures.is_empty() {
        log.json("failures", &failures(plan));
    }
}

/// Both tokens, their parts and both plans' witnesses, for a failure message.
fn shown(t0: &Precondition, t1: &Precondition, p0: &Plan, p1: &Plan) -> String {
    format!(
        "first {}\n{:#?}\n{:#}\nsecond {}\n{:#?}\n{:#}",
        t0.token(),
        t0.parts(),
        witnesses(p0),
        t1.token(),
        t1.parts(),
        witnesses(p1)
    )
}

fn run(log: &TestLog, name: &str, row: Row) {
    let mut case = (row.home)();
    log.env(&case.home.env);
    log.step("first plan");
    let (p0, t0) = within(name, || case.plan());
    logged(log, &p0, &t0);
    assert!(
        p0.failures.is_empty(),
        "{name}: the first plan failed: {:#}",
        failures(&p0)
    );
    log.step("mutate");
    log.note(row.what);
    (row.mutate)(&mut case);
    log.step("second plan");
    let (p1, t1) = within(name, || case.plan());
    logged(log, &p1, &t1);
    log.step("compare");
    let preview: Vec<String> = ["omaddons", "install", "--dry-run"]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    let changed: Vec<String> = match check(Some(&t0.token()), &t1, &preview) {
        Ok(()) => Vec::new(),
        Err(error) => error.details["changed"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect(),
    };
    log.json("changed", &changed);
    match row.expect {
        Expect::Equal => assert!(
            changed.is_empty(),
            "{name}: expected equal tokens, {changed:?} moved\n{}",
            shown(&t0, &t1, &p0, &p1)
        ),
        Expect::Moves(parts) => {
            let expected: Vec<&str> = parts.iter().map(|p| p.wire()).collect();
            assert_eq!(
                changed,
                expected,
                "{name}: these parts must move\n{}",
                shown(&t0, &t1, &p0, &p1)
            );
        }
        Expect::Fails { code, reason } => {
            let target = &p1.targets[0];
            let failed: Vec<_> = p1
                .failures
                .iter()
                .filter(|f| f.target == target.target)
                .collect();
            assert!(
                target.work.is_empty() && !failed.is_empty(),
                "{name}: expected {} to fail closed: {:#}",
                target.target,
                failures(&p1)
            );
            for f in failed {
                assert!(
                    f.error.code == code && f.error.details["reason"] == reason,
                    "{name}: expected {code:?}/{reason}: {:#}",
                    failures(&p1)
                );
            }
            assert_eq!(
                p1.witnesses.len(),
                p1.targets.len(),
                "{name}: one witness each"
            );
        }
    }
    (row.then)(log, &case, &p0, &p1);
}

// ---------------------------------------------------------------------------------------
// The matrix.

mod rows {
    use super::*;

    pub fn clean_file_edited() -> Row {
        row(
            managed,
            "Foo/Foo.lua rewritten with other bytes",
            |c| fs::write(foo(c, "Foo.lua"), "-- edited by hand\n").unwrap(),
            Expect::Moves(&[Part::Observed]),
        )
    }

    pub fn same_size_edit() -> Row {
        row(
            managed,
            "one byte of Foo/Foo.lua flipped, its size kept",
            |c| flip(&foo(c, "Foo.lua")),
            Expect::Moves(&[Part::Observed]),
        )
    }

    pub fn same_size_edit_with_mtime_restored() -> Row {
        row(
            managed,
            "one byte flipped, the mtime set back to the indexed one (only the ctime moved)",
            |c| {
                let path = foo(c, "Foo.lua");
                let mtime = fs::metadata(&path).unwrap().modified().unwrap();
                flip(&path);
                set_mtime(&path, mtime);
            },
            Expect::Moves(&[Part::Observed]),
        )
    }

    pub fn the_eleventh_changed_file() -> Row {
        row(
            || {
                let case = managed();
                // Ten of eleven already edited: the warning names those ten.
                for (rel, _) in FOO_FILES.iter().filter(|(rel, _)| *rel != "f08.lua") {
                    fs::write(foo(&case, rel), format!("-- {rel} edited\n")).unwrap();
                }
                case
            },
            "the eleventh file, f08.lua, edited too",
            |c| fs::write(foo(c, "f08.lua"), "-- 08 edited\n").unwrap(),
            Expect::Moves(&[Part::Observed]),
        )
    }

    pub fn file_deleted() -> Row {
        row(
            managed,
            "Foo/libs/lib.lua removed",
            |c| fs::remove_file(foo(c, "libs/lib.lua")).unwrap(),
            Expect::Moves(&[Part::Observed]),
        )
    }

    pub fn file_replaced_by_other_bytes() -> Row {
        row(
            managed,
            "another file with other bytes renamed over Foo/Foo.lua",
            |c| {
                let other = c.home.env.root().join("other.lua");
                fs::write(&other, "-- another file\n").unwrap();
                fs::rename(&other, foo(c, "Foo.lua")).unwrap();
            },
            Expect::Moves(&[Part::Observed]),
        )
    }

    pub fn file_replaced_by_identical_bytes() -> Row {
        row(
            managed,
            "an identical copy renamed over Foo/Foo.lua (a new inode, the same content)",
            |c| {
                let copy = c.home.env.root().join("copy.lua");
                fs::write(&copy, fs::read(foo(c, "Foo.lua")).unwrap()).unwrap();
                fs::rename(&copy, foo(c, "Foo.lua")).unwrap();
            },
            Expect::Equal,
        )
    }

    pub fn symlink_added() -> Row {
        row(
            managed,
            "Foo/link -> Foo.toc added",
            |c| std::os::unix::fs::symlink("Foo.toc", foo(c, "link")).unwrap(),
            Expect::Moves(&[Part::Observed]),
        )
    }

    pub fn empty_directory_added() -> Row {
        row(
            managed,
            "Foo/empty/ added",
            |c| fs::create_dir(foo(c, "empty")).unwrap(),
            Expect::Moves(&[Part::Observed]),
        )
    }

    pub fn empty_file_added() -> Row {
        row(
            managed,
            "Foo/empty.lua added",
            |c| fs::write(foo(c, "empty.lua"), "").unwrap(),
            Expect::Moves(&[Part::Observed]),
        )
    }

    pub fn touch_only() -> Row {
        row(
            managed,
            "every file of Foo given a new mtime, no byte changed",
            |c| {
                let later = SystemTime::now() + Duration::from_secs(3600);
                for path in files_under(&c.addons().join("Foo")) {
                    set_mtime(&path, later);
                }
            },
            Expect::Equal,
        )
    }

    pub fn chmod_only() -> Row {
        row(
            managed,
            "Foo/Foo.toc set to 0600",
            |c| {
                fs::set_permissions(foo(c, "Foo.toc"), fs::Permissions::from_mode(0o600)).unwrap();
            },
            Expect::Equal,
        )
    }

    pub fn unmanaged_file_edited() -> Row {
        row(
            unmanaged,
            "Bar/Bar.lua edited",
            |c| fs::write(bar(c, "Bar.lua"), "-- edited\n").unwrap(),
            Expect::Moves(&[Part::Observed]),
        )
    }

    pub fn unmanaged_touch_only() -> Row {
        row(
            unmanaged,
            "every file of Bar given a new mtime",
            |c| {
                let later = SystemTime::now() + Duration::from_secs(3600);
                for path in files_under(&c.addons().join("Bar")) {
                    set_mtime(&path, later);
                }
            },
            Expect::Equal,
        )
    }

    pub fn a_created_name_appears() -> Row {
        row(
            || {
                let mut case = managed();
                case.release("baz", "v1", "Baz", &[("Baz.toc", TOC)]);
                case.requests = |c| {
                    vec![request(
                        &c.retail,
                        vec![update("foo"), install("gh:o/baz")],
                        TxKind::Install,
                    )]
                };
                case
            },
            "a hand-made Baz/ created where the install adds Baz",
            |c| {
                c.home.hand_made(
                    &c.retail,
                    "Baz",
                    &[("Baz.toc", TOC), ("baz.lua", "-- mine\n")],
                );
            },
            Expect::Moves(&[Part::Selection, Part::Observed]),
        )
    }

    pub fn the_addons_directory_appears() -> Row {
        row(
            fresh,
            "Interface/AddOns created empty",
            |c| fs::create_dir_all(c.addons()).unwrap(),
            Expect::Moves(&[Part::Selection, Part::Observed]),
        )
    }

    pub fn copy_source_edited_record_unchanged() -> Row {
        row(
            copy,
            "the source client's Foo/Foo.lua edited, no record touched",
            |c| fs::write(c.other.addons_dir().join("Foo/Foo.lua"), "-- edited\n").unwrap(),
            Expect::Moves(&[Part::Observed]),
        )
    }

    pub fn adopted_folder_edited() -> Row {
        row(
            adopt,
            "Hand/Hand.lua edited",
            |c| fs::write(c.addons().join("Hand/Hand.lua"), "-- edited\n").unwrap(),
            Expect::Moves(&[Part::Observed]),
        )
    }

    pub fn marker_rewritten() -> Row {
        row(
            managed,
            "target.json rewritten with the same key, pretty-printed",
            |c| {
                let marker = omaddons::discover::registry::marker_path(&c.retail.client_dir);
                let key = c.retail.key.map(|k| k.to_string()).unwrap_or_default();
                fs::write(
                    marker,
                    format!("{{\n  \"schema_version\": \"1.0\",\n  \"key\": \"{key}\"\n}}\n"),
                )
                .unwrap();
            },
            Expect::Moves(&[Part::Observed]),
        )
    }

    pub fn target_registered_between_plans() -> Row {
        row(
            unmanaged,
            "the target registered (record and marker) between the plans",
            |c| c.home.manage(&mut c.retail, &[]),
            Expect::Moves(&[Part::Targets, Part::Selection, Part::Observed]),
        )
    }

    pub fn another_process_and_clock() -> Row {
        row(
            managed,
            "nothing: the second plan draws its own transaction and time",
            |_| {},
            Expect::Equal,
        )
    }

    pub fn unrelated_folder_edited() -> Row {
        row(
            || {
                let case = managed();
                case.home.hand_made(
                    &case.retail,
                    "Other",
                    &[("Other.toc", TOC), ("other.lua", "-- o\n")],
                );
                case
            },
            "Other/, which the plan does not touch, edited",
            |c| fs::write(c.addons().join("Other/other.lua"), "-- edited\n").unwrap(),
            Expect::Equal,
        )
    }

    pub fn saved_variables_edited() -> Row {
        Row {
            then: |_, _, p0, p1| {
                for plan in [p0, p1] {
                    for w in &plan.witnesses {
                        assert!(
                            w.folders.iter().all(|f| !f.key.contains("wtf")),
                            "a witnessed path in WTF/: {:?}",
                            w.folders
                        );
                    }
                }
            },
            ..row(
                managed,
                "WTF/Account/FAKEACCT/SavedVariables/Foo.lua written",
                |c| {
                    let sv = c
                        .retail
                        .client_dir
                        .join("WTF/Account/FAKEACCT/FakeRealm/FakeChar/SavedVariables");
                    fs::create_dir_all(&sv).unwrap();
                    fs::write(sv.join("Foo.lua"), "FooDB = {}\n").unwrap();
                },
                Expect::Equal,
            )
        }
    }

    pub fn unreadable_file_fails_closed() -> Row {
        Row {
            then: |log, c, p0, p1| {
                // The other target plans and witnesses as before.
                let other = TargetRef::of(c.other.key, &c.other.id).to_string();
                let token = |p: &Plan| c.home.token("install", p, &PlanExtras::default());
                let part = |p: &Plan, i: usize| -> Value {
                    serde_json::from_str(&token(p).parts()[i]).unwrap()
                };
                let (s0, s1) = (part(p0, 2)[&other].clone(), part(p1, 2)[&other].clone());
                let (w0, w1) = (
                    part(p0, 3)["witnesses"][&other].clone(),
                    part(p1, 3)["witnesses"][&other].clone(),
                );
                log.json(
                    "the other target",
                    &json!({"selection": [&s0, &s1], "witness": [&w0, &w1]}),
                );
                assert!(
                    s0 == s1 && !s0.is_null(),
                    "{other}'s selection moved: {s0} vs {s1}"
                );
                assert!(
                    w0 == w1 && !w0.is_null(),
                    "{other}'s witness moved: {w0} vs {w1}"
                );
            },
            ..row(
                || {
                    let mut case = unmanaged();
                    let zip = case.home.env.root().join("dl/Leg.zip");
                    fs::create_dir_all(zip.parent().unwrap()).unwrap();
                    fs::write(
                        &zip,
                        common::archives::zip_text(&[("Leg/Leg.toc", "## Interface: 30300\n")]),
                    )
                    .unwrap();
                    case.requests = |c| {
                        let mut leg = install("gh:o/leg");
                        if let RequestedChange::Install { file, .. } = &mut leg {
                            *file = Some(c.home.env.root().join("dl/Leg.zip"));
                        }
                        vec![
                            request(&c.retail, vec![install("gh:o/bar")], TxKind::Install),
                            request(&c.other, vec![leg], TxKind::Install),
                        ]
                    };
                    case
                },
                "Bar/secret.lua added with mode 0o000",
                |c| {
                    let secret = bar(c, "secret.lua");
                    fs::write(&secret, "-- secret\n").unwrap();
                    fs::set_permissions(&secret, fs::Permissions::from_mode(0o000)).unwrap();
                },
                Expect::Fails {
                    code: ErrorCode::Io,
                    reason: "unindexable",
                },
            )
        }
    }

    pub fn entry_limit_fails_closed() -> Row {
        row(
            || {
                let mut case = unmanaged();
                case.opts.limits = ArchiveLimits {
                    max_entries: 4,
                    ..ArchiveLimits::default()
                };
                case
            },
            "Bar given 8 entries (the limit is 4)",
            |c| {
                for n in 0..6 {
                    fs::write(bar(c, &format!("extra{n}.lua")), "--\n").unwrap();
                }
            },
            Expect::Fails {
                code: ErrorCode::ArchiveTooLarge,
                reason: "limit_exceeded",
            },
        )
    }

    pub fn byte_limit_fails_closed_reused_bytes_free() -> Row {
        row(
            || {
                let mut case = managed();
                case.home.hand_made(&case.retail, "Bar", &BAR_FILES);
                case.release("bar", "v1", "Bar", &[("Bar.toc", TOC)]);
                case.opts.force = true;
                // Foo's 86 reused bytes are over it; Bar's 32 hashed ones are not.
                case.opts.limits = ArchiveLimits {
                    max_uncompressed: 64,
                    ..ArchiveLimits::default()
                };
                case.requests = |c| {
                    vec![request(
                        &c.retail,
                        vec![update("foo"), install("gh:o/bar")],
                        TxKind::Install,
                    )]
                };
                case
            },
            "Bar/big.lua of 128 bytes added",
            |c| fs::write(bar(c, "big.lua"), "-".repeat(128)).unwrap(),
            Expect::Fails {
                code: ErrorCode::ArchiveTooLarge,
                reason: "limit_exceeded",
            },
        )
    }

    /// `unmanaged` with a FIFO `Bar/pipe`.
    fn with_fifo() -> Case {
        let case = unmanaged();
        mkfifo(&bar(&case, "pipe"));
        case
    }

    pub fn fifo_is_never_opened() -> Row {
        row(with_fifo, "nothing", |_| {}, Expect::Equal)
    }

    pub fn fifo_replaced_by_a_file() -> Row {
        row(
            with_fifo,
            "the FIFO Bar/pipe replaced by a regular file",
            |c| {
                fs::remove_file(bar(c, "pipe")).unwrap();
                fs::write(bar(c, "pipe"), "-- a file\n").unwrap();
            },
            Expect::Moves(&[Part::Observed]),
        )
    }

    pub fn symlink_to_root_is_never_followed() -> Row {
        Row {
            then: |_, _, p0, p1| {
                for plan in [p0, p1] {
                    let hashed = plan.witnesses[0].cost().files_hashed;
                    assert_eq!(hashed, 2, "only Bar's own two files: {:#}", witnesses(plan));
                }
            },
            ..row(
                || {
                    let case = unmanaged();
                    std::os::unix::fs::symlink("/", bar(&case, "escape")).unwrap();
                    case
                },
                "nothing",
                |_| {},
                Expect::Equal,
            )
        }
    }

    pub fn symlink_loop_is_never_followed() -> Row {
        row(
            || {
                let case = unmanaged();
                std::os::unix::fs::symlink(".", bar(&case, "loop")).unwrap();
                case
            },
            "nothing",
            |_| {},
            Expect::Equal,
        )
    }

    pub fn non_utf8_name_renamed() -> Row {
        row(
            || {
                let case = unmanaged();
                let name = OsStr::from_bytes(b"\xff.lua");
                fs::write(case.addons().join("Bar").join(name), "-- x\n").unwrap();
                case
            },
            "Bar/\\xff.lua renamed to Bar/\\xfe.lua",
            |c| {
                let dir = c.addons().join("Bar");
                fs::rename(
                    dir.join(OsStr::from_bytes(b"\xff.lua")),
                    dir.join(OsStr::from_bytes(b"\xfe.lua")),
                )
                .unwrap();
            },
            Expect::Moves(&[Part::Observed]),
        )
    }
}

/// One `#[test]` per row, named as the row, each with its own test log.
macro_rules! matrix {
    ($($name:ident),* $(,)?) => {
        $(
            #[test]
            fn $name() {
                run(&tlog!(), stringify!($name), rows::$name());
            }
        )*
        /// The rows, by name, for scenarios that reuse them.
        fn row_named(name: &str) -> Row {
            match name {
                $(stringify!($name) => rows::$name(),)*
                _ => panic!("no row {name}"),
            }
        }
        const ROWS: &[&str] = &[$(stringify!($name)),*];
    };
}

matrix! {
    clean_file_edited,
    same_size_edit,
    same_size_edit_with_mtime_restored,
    the_eleventh_changed_file,
    file_deleted,
    file_replaced_by_other_bytes,
    file_replaced_by_identical_bytes,
    symlink_added,
    empty_directory_added,
    empty_file_added,
    touch_only,
    chmod_only,
    unmanaged_file_edited,
    unmanaged_touch_only,
    a_created_name_appears,
    the_addons_directory_appears,
    copy_source_edited_record_unchanged,
    adopted_folder_edited,
    marker_rewritten,
    target_registered_between_plans,
    another_process_and_clock,
    unrelated_folder_edited,
    saved_variables_edited,
    unreadable_file_fails_closed,
    entry_limit_fails_closed,
    byte_limit_fails_closed_reused_bytes_free,
    fifo_is_never_opened,
    fifo_replaced_by_a_file,
    symlink_to_root_is_never_followed,
    symlink_loop_is_never_followed,
    non_utf8_name_renamed,
}

#[test]
fn the_matrix_has_its_31_rows() {
    assert_eq!(ROWS.len(), 31, "{ROWS:?}");
}

// ---------------------------------------------------------------------------------------
// Outside the table.

/// What lies under `dir`, from metadata alone (never opening a file: a FIFO or a mode
/// 000 file is listed like any other): each path with its kind, size, mode and mtime.
fn listing(dir: &Path) -> Vec<String> {
    fn walk(dir: &Path, out: &mut Vec<String>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries {
            let path = entry.unwrap().path();
            let meta = fs::symlink_metadata(&path).unwrap();
            out.push(format!(
                "{} {:o} {} {}.{}",
                path.display(),
                meta.mode(),
                meta.len(),
                meta.mtime(),
                meta.mtime_nsec()
            ));
            if meta.is_dir() {
                walk(&path, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, &mut out);
    out.sort();
    out
}

/// The witness and every fail-closed path write nothing: the records and files index
/// (`<state>/targets`) and every client dir are the same before and after the second
/// plan. `net.log` and the in-flight locks under `<state>` belong to the requests a
/// plan makes, not to the witness.
#[test]
fn collection_writes_nothing() {
    let log = tlog!();
    for name in [
        "clean_file_edited",
        "unmanaged_file_edited",
        "unreadable_file_fails_closed",
        "entry_limit_fails_closed",
        "fifo_is_never_opened",
    ] {
        let case_log = log.case(name);
        let row = row_named(name);
        let mut case = (row.home)();
        case_log.env(&case.home.env);
        case_log.step("first plan");
        within(name, || case.plan());
        case_log.step("mutate");
        (row.mutate)(&mut case);
        let records = case.home.paths.targets_dir();
        fs::create_dir_all(&records).unwrap();
        case_log.tree("records", &records);
        let clients = [
            case.retail.client_dir.clone(),
            case.other.client_dir.clone(),
        ];
        let before: Vec<Vec<String>> = clients.iter().map(|c| listing(c)).collect();
        case_log.step("second plan");
        within(name, || case.plan());
        case_log.assert_unchanged("records", &records);
        let after: Vec<Vec<String>> = clients.iter().map(|c| listing(c)).collect();
        case_log.assert_eq("every client dir", &after, &before);
    }
}

/// Settled folders keep their private stamps, and collecting again agrees: three plans
/// past the racy slack give one token, the same reuse, and a stamp for every witnessed
/// folder.
#[test]
fn repeated_collection_agrees() {
    let log = tlog!();
    let mut case = managed();
    case.home.hand_made(&case.retail, "Bar", &BAR_FILES);
    case.release("bar", "v1", "Bar", &[("Bar.toc", TOC)]);
    case.opts.force = true;
    case.requests = |c| {
        vec![request(
            &c.retail,
            vec![update("foo"), install("gh:o/bar")],
            TxKind::Install,
        )]
    };
    log.env(&case.home.env);
    // Past RACY_SLACK_NS, so every folder has settled.
    std::thread::sleep(Duration::from_millis(50));
    let mut runs = Vec::new();
    for step in ["first plan", "second plan", "third plan"] {
        log.step(step);
        let (plan, token) = case.plan();
        logged(&log, &plan, &token);
        runs.push((plan, token));
    }
    log.step("compare");
    let tokens: Vec<String> = runs.iter().map(|(_, t)| t.token()).collect();
    log.assert_eq(
        "three equal tokens",
        &tokens[1..].to_vec(),
        &vec![tokens[0].clone(); 2],
    );
    let reused: Vec<u64> = runs
        .iter()
        .map(|(p, _)| p.witnesses[0].cost().files_reused)
        .collect();
    log.assert_eq(
        "the same reuse each time",
        &reused[1..].to_vec(),
        &vec![reused[0]; 2],
    );
    let third = &runs[2].0.witnesses[0];
    let stamped: Vec<(String, bool)> = third
        .folders
        .iter()
        .map(|f| {
            let dir = case
                .addons()
                .join(if f.key == "foo" { "Foo" } else { "Bar" });
            (f.key.clone(), third.stamp(&dir).is_some())
        })
        .collect();
    log.assert_eq(
        "every witnessed folder keeps a stamp",
        &stamped,
        &vec![("bar".to_owned(), true), ("foo".to_owned(), true)],
    );
}
