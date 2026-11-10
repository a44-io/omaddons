//! e2e suite: `targets rm --purge` (omad-launch-week-fixes-bmcx.2): deleting what omaddons kept for a client Battle.net uninstalled, previewed, bound and crash-safe. Every multi-command scenario is a TestLog scenario.
//!
//! A home with retail and Mists Classic: Foo installed on classic and updated (an undo
//! snapshot with a state half), a backup of classic's `WTF/`, and Bar on retail; then
//! Battle.net uninstalls classic, leaving its `Interface/` (AddOns and `.omaddons`), fonts,
//! loader DLLs and Proton's shader cache. The purge preview lists what goes and changes
//! nothing; a run needs `--yes`, binds to its preview, deletes exactly `.omaddons`, the
//! state halves and the record, and unregisters the client last; a crash at any step
//! leaves the client registered, and the next purge finishes the job.

use crate::common;

use std::fs;
use std::path::{Path, PathBuf};

use common::tlog::tlog;
use serde_json::json;
use support::{backups_whole_or_gone, deletions, registered, uninstalled_classic};

#[cfg(test)]
mod support {
    use super::*;

    use common::addons::toc;
    use common::home::Home;
    use common::http::Asset;
    use common::tlog::TestLog;
    use serde_json::Value;

    /// The scenario's home, classic uninstalled; see the module docs.
    pub fn uninstalled_classic(log: &TestLog) -> Home {
        log.step("a home whose classic client Battle.net uninstalled");
        let mut home = Home::new(&["wow", "wow_classic"]);
        let (t1, t2) = (
            toc("50504", &["## Version: v1"]),
            toc("50504", &["## Version: v2"]),
        );
        let a1: [Asset<'_>; 1] = [("Foo.zip", &[], vec![("Foo/Foo.toc", t1.as_str())])];
        let a2: [Asset<'_>; 1] = [("Foo.zip", &[], vec![("Foo/Foo.toc", t2.as_str())])];
        home.installed_behind(
            "https://github.com/o/foo",
            ("v1", &a1),
            ("v2", &a2),
            &["classic"],
        );
        log.run(&home, &["update", "-t", "classic", "--json"]).ok();
        log.run(&home, &["backup", "create", "-t", "classic", "--json"])
            .ok();
        let bar = home.env.root().join("addons/Bar");
        fs::create_dir_all(&bar).unwrap();
        fs::write(bar.join("Bar.toc"), toc("120105", &[])).unwrap();
        let bar = bar.to_string_lossy().into_owned();
        log.run(&home, &["install", &bar, "-t", "retail", "--json"])
            .ok();
        home.uninstall("_classic_", "wow_classic");
        // Discovery notices the uninstall once and caches it (derived data), so the trees
        // below compare only what the purge itself changes.
        log.run(&home, &["targets", "--json"]).ok();
        home
    }

    /// `data.purge.deletions` as `(kind, path)`.
    pub fn deletions(data: &Value) -> Vec<(String, PathBuf)> {
        data["purge"]["deletions"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|d| {
                (
                    d["kind"].as_str().unwrap_or_default().to_owned(),
                    PathBuf::from(d["path"].as_str().unwrap_or_default()),
                )
            })
            .collect()
    }

    /// The registered ids, from `targets --json`.
    pub fn registered(log: &TestLog, home: &Home) -> Vec<String> {
        let ran = log.run(home, &["targets", "--json"]);
        ran.ok();
        ran.data()["targets"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|t| t["registered"] == true)
            .map(|t| t["id"].as_str().unwrap_or_default().to_owned())
            .collect()
    }

    /// Every backup directory under `meta` is whole (its `backup.json` and manifest are
    /// there) or gone: none is ever half deleted at its own path.
    pub fn backups_whole_or_gone(meta: &Path) -> bool {
        let Ok(entries) = fs::read_dir(meta.join("backups")) else {
            return true;
        };
        entries.flatten().all(|entry| {
            let dir = entry.path();
            dir.join("backup.json").is_file() && dir.join("manifest.json").is_file()
        })
    }
}

#[test]
fn a_purge_previews_then_deletes_exactly_what_it_listed() {
    let log = tlog!();
    let home = uninstalled_classic(&log);
    let root = home.env.root().to_path_buf();
    let classic = home.client("_classic_");
    let meta = classic.join("Interface/.omaddons");
    let history = home.env.state().join("omaddons/history.jsonl");
    let lines_before = fs::read_to_string(&history).unwrap();
    log.tree("home", &root);
    log.tree("addons", &classic.join("Interface/AddOns"));
    log.tree("retail", &home.client("_retail_"));

    log.step("the preview lists what goes and changes nothing");
    let preview = log.run(
        &home,
        &["targets", "rm", "classic", "--purge", "--dry-run", "--json"],
    );
    preview.ok();
    let data = preview.data().clone();
    log.json("preview", &data);
    let kinds: Vec<String> = deletions(&data).into_iter().map(|(k, _)| k).collect();
    for kind in ["marker", "backup", "snapshot", "state_snapshot", "record"] {
        assert!(kinds.contains(&kind.to_owned()), "{kind} in {kinds:?}");
    }
    assert_eq!(data["dry_run"], true);
    assert_eq!(data["purge"]["backups"].as_array().map(Vec::len), Some(1));
    assert_eq!(data["purge"]["kept_entries"], json!([]));
    assert!(
        data["purge"]["lost_undos"]
            .as_array()
            .is_some_and(|txs| !txs.is_empty())
    );
    assert!(data["purge"]["bytes"].as_u64().is_some_and(|b| b > 0));
    let token = data["precondition"].as_str().unwrap().to_owned();
    // Nothing outside .omaddons, the state halves and the records.
    for (_, path) in deletions(&data) {
        assert!(
            path.starts_with(&meta) || path.starts_with(home.env.state()),
            "{path:?}"
        );
    }
    log.assert_unchanged("home", &root);

    log.step("the human preview names the backups and what can no longer be undone");
    let human = log.run(&home, &["targets", "rm", "classic", "--purge", "--dry-run"]);
    human.ok();
    let text = human.stdout();
    assert!(
        text.contains("would delete what omaddons kept for classic"),
        "{text}"
    );
    assert!(text.contains("can no longer be undone"), "{text}");
    assert!(text.contains("--yes --if-match"), "{text}");
    log.assert_unchanged("home", &root);

    log.step("a real run without --yes is refused, naming the preview");
    let refused = log.run(&home, &["targets", "rm", "classic", "--purge", "--json"]);
    refused.fails(2, "usage");
    log.assert_eq(
        "the fix",
        &refused.envelope()["error"]["fix_argv"],
        &json!([
            "omaddons",
            "targets",
            "rm",
            "classic",
            "--purge",
            "--dry-run"
        ]),
    );
    log.assert_unchanged("home", &root);

    log.step("the bound run deletes exactly the preview and unregisters classic");
    let ran = log.run(
        &home,
        &[
            "targets",
            "rm",
            "classic",
            "--purge",
            "--yes",
            "--if-match",
            &token,
            "--json",
        ],
    );
    ran.ok();
    log.assert_eq("what it deleted", &deletions(ran.data()), &deletions(&data));
    assert_eq!(ran.data()["dry_run"], false);
    assert!(!meta.exists(), ".omaddons is gone");
    for (_, path) in deletions(&data) {
        assert!(fs::symlink_metadata(&path).is_err(), "{path:?} is gone");
    }
    log.assert_unchanged("addons", &classic.join("Interface/AddOns"));
    log.assert_unchanged("retail", &home.client("_retail_"));
    for left in ["Fonts", "Wow_loader.dll", "vkd3d-proton.cache"] {
        assert!(classic.join(left).exists(), "{left} is the launcher's");
    }
    // History is never pruned: the purge appends its targets line.
    let lines_after = fs::read_to_string(&history).unwrap();
    assert!(lines_after.starts_with(&lines_before));
    let added: Vec<&str> = lines_after[lines_before.len()..].lines().collect();
    assert_eq!(added.len(), 1, "{added:?}");
    assert!(added[0].contains("\"--purge\""), "{}", added[0]);
    log.assert_eq(
        "registered",
        &registered(&log, &home),
        &vec!["retail".to_owned()],
    );
    let status = log.run(&home, &["status", "--json"]);
    status.ok();
    let ids: Vec<&str> = status.data()["targets"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| t["id"].as_str())
        .collect();
    log.assert_eq("status", &ids, &vec!["retail"]);

    log.step("a second purge finds no such client");
    log.run(
        &home,
        &["targets", "rm", "classic", "--purge", "--yes", "--json"],
    )
    .fails(3, "target_not_found");
}

/// omad-hz6h: a purge's backups can be kept first. `backup list -t` and `backup export`
/// reach the backups of a client Battle.net uninstalled, read-only; the export imports
/// elsewhere; every change naming the client is still refused, and nothing in its
/// folder changes until the purge.
#[test]
fn a_backup_of_a_client_no_longer_installed_is_listed_and_exported_before_the_purge() {
    let log = tlog!();
    let home = uninstalled_classic(&log);
    let classic = home.client("_classic_");
    log.tree("classic", &classic);

    log.step("backup list -t classic lists its backup, by id and by key");
    let listed = log.run(&home, &["backup", "list", "-t", "classic", "--json"]);
    listed.ok();
    let targets = listed.data()["targets"].as_array().unwrap().clone();
    assert_eq!(targets.len(), 1, "{targets:?}");
    assert_eq!(targets[0]["target"]["id"], "classic");
    assert_eq!(targets[0]["total"], 1);
    let id = targets[0]["backups"][0]["id"].as_str().unwrap().to_owned();
    let key = targets[0]["target"]["key"].as_str().unwrap().to_owned();
    // A client no longer installed takes no new backup, so none is recommended.
    let actions: Vec<String> = listed.commands().into_iter().map(|c| c.action).collect();
    log.assert_eq("its next steps", &actions, &Vec::<String>::new());
    let by_key = log.run(&home, &["backup", "list", "-t", &key, "--json"]);
    by_key.ok();
    log.assert_eq(
        "by key",
        &by_key.data()["targets"][0]["backups"][0]["id"],
        &json!(id),
    );
    let one = log.run(
        &home,
        &["backup", "list", &id[..10], "-t", "classic", "--json"],
    );
    one.ok();
    assert_eq!(one.data()["targets"][0]["shown"], 1);
    let human = log.run(&home, &["backup", "list", "-t", "classic"]);
    human.ok();
    assert!(
        human
            .stdout()
            .contains("classic (no longer installed) · 1 backup"),
        "{}",
        human.stdout()
    );

    log.step("backup export writes it off the client, with -t and without");
    let out = home.env.root().join("keep");
    fs::create_dir_all(&out).unwrap();
    let file = out.join("classic-wtf.tar.zst");
    let file_arg = file.to_string_lossy().into_owned();
    let exported = log.run(
        &home,
        &[
            "backup", "export", &id, "-t", "classic", &file_arg, "--json",
        ],
    );
    exported.ok();
    assert_eq!(exported.data()["target"]["id"], "classic");
    assert_eq!(exported.data()["target"]["key"], json!(key));
    assert!(file.is_file());
    let again = out.join("again.tar.zst");
    log.run(
        &home,
        &["backup", "export", &id, &again.to_string_lossy(), "--json"],
    )
    .ok();
    assert_eq!(
        fs::read(&file).unwrap(),
        fs::read(&again).unwrap(),
        "one backup always exports to the same bytes"
    );

    log.step("never into the folder of a client no longer installed");
    let inside = classic.join("Fonts/wtf.tar.zst");
    let refused = log.run(
        &home,
        &[
            "backup",
            "export",
            &id,
            "-t",
            "classic",
            &inside.to_string_lossy(),
            "--json",
        ],
    );
    refused.fails(4, "scope_denied");
    let message = refused.envelope()["error"]["message"].as_str().unwrap();
    assert!(message.contains("inside the game client"), "{message}");
    log.assert_unchanged("classic", &classic);

    log.step("every change naming classic is still refused, and touches nothing");
    let bar = home.env.root().join("addons/Bar");
    let bar = bar.to_string_lossy().into_owned();
    for argv in [
        &["install", bar.as_str(), "-t", "classic", "--json"][..],
        &["update", "-t", "classic", "--json"],
        &["backup", "create", "-t", "classic", "--json"],
        &["backup", "restore", id.as_str(), "-t", "classic", "--json"],
        &["backup", "verify", id.as_str(), "-t", "classic", "--json"],
        &["backup", "prune", "-t", "classic", "--json"],
    ] {
        log.run(&home, argv).fails(3, "target_not_found");
    }
    log.assert_unchanged("classic", &classic);

    log.step("the export imports on another client");
    let import = [
        "backup",
        "import",
        file_arg.as_str(),
        "-t",
        "retail",
        "--json",
    ];
    log.run(&home, &import).fails(4, "wrong_flavor");
    let forced: Vec<&str> = [&import[..], &["--force"][..]].concat();
    let imported = log.run(&home, &forced);
    imported.ok();
    assert_eq!(imported.data()["backup"]["kind"], "imported");
    assert_eq!(imported.data()["source"]["id"], json!(id));
    assert_eq!(imported.data()["source"]["target_id"], "classic");

    log.step("the purge preview names the export of each backup");
    let preview = log.run(&home, &["targets", "rm", "classic", "--purge", "--dry-run"]);
    preview.ok();
    let text = preview.stdout();
    assert!(
        text.contains(&format!(
            "keep it: omaddons backup export {id} -t classic <file>"
        )),
        "{text}"
    );
    log.assert_unchanged("classic", &classic);

    log.step("after the purge, classic is no client at all, and its export stays");
    log.run(
        &home,
        &["targets", "rm", "classic", "--purge", "--yes", "--json"],
    )
    .ok();
    log.run(&home, &["backup", "list", "-t", "classic", "--json"])
        .fails(3, "target_not_found");
    let retail = log.run(&home, &["backup", "list", "-t", "retail", "--json"]);
    retail.ok();
    assert_eq!(retail.data()["targets"][0]["total"], 1);
    assert!(file.is_file());
}

/// omad-hz6h: a registered client whose folder is gone entirely has no backups to
/// reach: `backup list -t` says so, and an export finds no such backup, rather than
/// failing on the missing path.
#[test]
fn a_client_whose_folder_is_gone_lists_no_backups() {
    let log = tlog!();
    let home = uninstalled_classic(&log);
    let classic = home.client("_classic_");
    fs::remove_dir_all(&classic).unwrap();

    log.step("backup list -t classic lists none");
    let listed = log.run(&home, &["backup", "list", "-t", "classic", "--json"]);
    listed.ok();
    let target = &listed.data()["targets"][0];
    assert_eq!(target["target"]["id"], "classic");
    assert_eq!(target["total"], 0);
    assert_eq!(target["backups"], json!([]));
    let human = log.run(&home, &["backup", "list", "-t", "classic"]);
    human.ok();
    assert!(
        human
            .stdout()
            .contains("classic (no longer installed) · no backups"),
        "{}",
        human.stdout()
    );

    log.step("an export of a backup it no longer has is not_found");
    let file = home.env.root().join("wtf.tar.zst");
    log.run(
        &home,
        &[
            "backup",
            "export",
            "01JB8Q2VA0",
            "-t",
            "classic",
            &file.to_string_lossy(),
            "--json",
        ],
    )
    .fails(3, "not_found");
    assert!(!classic.exists(), "nothing made its folder again");
    assert!(!file.exists());
}

#[test]
fn a_stale_preview_deletes_nothing() {
    let log = tlog!();
    let home = uninstalled_classic(&log);
    let root = home.env.root().to_path_buf();
    let meta = home.client("_classic_").join("Interface/.omaddons");

    log.step("a preview, then a new snapshot directory");
    let preview = log.run(
        &home,
        &["targets", "rm", "classic", "--purge", "--dry-run", "--json"],
    );
    preview.ok();
    let token = preview.data()["precondition"].as_str().unwrap().to_owned();
    fs::create_dir_all(meta.join("snapshots/01JB8Q2VA00000000000000000/AddOns")).unwrap();
    log.tree("home", &root);

    log.step("the bound run is stale and deletes nothing");
    log.run(
        &home,
        &[
            "targets",
            "rm",
            "classic",
            "--purge",
            "--yes",
            "--if-match",
            &token,
            "--json",
        ],
    )
    .fails(1, "plan_stale");
    // It took the lock (its holder line is all that changed) and deleted nothing.
    let diff = log.diff("home", &root);
    log.assert_eq(
        "what changed",
        &(diff.added, diff.removed, diff.changed),
        &(
            Vec::new(),
            Vec::new(),
            vec![("state/omaddons/lock".to_owned(), "content".to_owned())],
        ),
    );
    log.assert_eq(
        "registered",
        &registered(&log, &home),
        &vec!["classic".to_owned(), "retail".to_owned()],
    );
}

#[test]
fn an_installed_client_and_stray_flags_are_refused_before_anything_changes() {
    let log = tlog!();
    let home = uninstalled_classic(&log);
    let root = home.env.root().to_path_buf();
    log.tree("home", &root);

    log.step("--purge on an installed client: the plain rm is the fix");
    for argv in [
        &["targets", "rm", "retail", "--purge", "--dry-run", "--json"][..],
        &["targets", "rm", "retail", "--purge", "--yes", "--json"],
    ] {
        let ran = log.run(&home, argv);
        ran.fails(2, "usage");
        log.assert_eq(
            "the fix",
            &ran.envelope()["error"]["fix_argv"],
            &json!(["omaddons", "targets", "rm", "retail"]),
        );
    }

    log.step("--yes needs --purge");
    log.run(&home, &["targets", "rm", "classic", "--yes", "--json"])
        .exit(2);
    log.assert_unchanged("home", &root);

    log.step("a plain rm binds to its preview too: a purge's token is stale for it");
    let purge = log.run(
        &home,
        &["targets", "rm", "classic", "--purge", "--dry-run", "--json"],
    );
    purge.ok();
    let purge_token = purge.data()["precondition"].as_str().unwrap().to_owned();
    log.run(
        &home,
        &[
            "targets",
            "rm",
            "classic",
            "--if-match",
            &purge_token,
            "--json",
        ],
    )
    .fails(1, "plan_stale");
    let forget = log.run(&home, &["targets", "rm", "classic", "--dry-run", "--json"]);
    forget.ok();
    let token = forget.data()["precondition"].as_str().unwrap().to_owned();
    log.run(
        &home,
        &["targets", "rm", "classic", "--if-match", &token, "--json"],
    )
    .ok();
    log.assert_eq(
        "registered",
        &registered(&log, &home),
        &vec!["retail".to_owned()],
    );
}

/// A crash (`abort`) or an injected I/O failure (`error`) at each step of a purge leaves
/// the client registered, nothing deleted or every victim out of its place, and no
/// backup half deleted; the next purge finishes.
#[test]
fn a_crash_at_any_step_leaves_a_purge_the_next_one_finishes() {
    let log = tlog!();
    let steps = [
        "targets.purge.planned",
        "fsx.prune.renamed",
        "fsx.prune.deleting",
        "targets.purge.emptied",
    ];
    for (failpoint, mode) in steps
        .iter()
        .flat_map(|step| [(*step, "abort"), (*step, "error")])
    {
        let case = log.case(&format!("{failpoint}-{mode}"));
        let home = uninstalled_classic(&case);
        let meta = home.client("_classic_").join("Interface/.omaddons");
        let preview = case.run(
            &home,
            &["targets", "rm", "classic", "--purge", "--dry-run", "--json"],
        );
        preview.ok();
        let planned = deletions(preview.data());

        case.step("the purge crashes, or fails");
        let ran = case
            .cmd(
                &home,
                &["targets", "rm", "classic", "--purge", "--yes", "--json"],
            )
            .failpoints(&format!("{failpoint}={mode}"))
            .run();
        if mode == "abort" {
            ran.aborted();
        } else {
            ran.fails(1, "io");
        }
        let present: Vec<bool> = planned
            .iter()
            .filter(|(kind, _)| kind != "trash")
            .map(|(_, path)| fs::symlink_metadata(path).is_ok())
            .collect();
        case.json("still in place", &json!(present));
        assert!(
            present.iter().all(|p| *p) || present.iter().all(|p| !*p),
            "{failpoint}: all or nothing moved: {present:?}"
        );
        assert!(backups_whole_or_gone(&meta), "{failpoint}");
        case.assert_eq(
            "still registered",
            &registered(&case, &home),
            &vec!["classic".to_owned(), "retail".to_owned()],
        );

        case.step("the next purge finishes");
        case.run(
            &home,
            &["targets", "rm", "classic", "--purge", "--yes", "--json"],
        )
        .ok();
        assert!(!meta.exists(), "{failpoint}");
        for (_, path) in &planned {
            assert!(fs::symlink_metadata(path).is_err(), "{failpoint}: {path:?}");
        }
        case.assert_eq(
            "unregistered",
            &registered(&case, &home),
            &vec!["retail".to_owned()],
        );
    }
}

/// What omaddons did not write under `.omaddons` is listed as kept and never deleted, so
/// `.omaddons` stays; the client is still unregistered.
#[test]
fn what_omaddons_did_not_write_is_kept() {
    let log = tlog!();
    let home = uninstalled_classic(&log);
    let meta = home.client("_classic_").join("Interface/.omaddons");
    let notes = meta.join("notes.txt");
    fs::write(&notes, "mine").unwrap();

    log.step("the preview keeps it");
    let preview = log.run(
        &home,
        &["targets", "rm", "classic", "--purge", "--dry-run", "--json"],
    );
    preview.ok();
    log.assert_eq(
        "kept",
        &preview.data()["purge"]["kept_entries"],
        &json!([notes.to_string_lossy()]),
    );

    log.step("the purge leaves it, and .omaddons with it");
    log.run(
        &home,
        &["targets", "rm", "classic", "--purge", "--yes", "--json"],
    )
    .ok();
    assert_eq!(fs::read_to_string(&notes).unwrap(), "mine");
    let left: Vec<PathBuf> = fs::read_dir(&meta)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect();
    log.assert_eq("left", &left, &vec![notes.clone()]);
    log.assert_eq(
        "registered",
        &registered(&log, &home),
        &vec!["retail".to_owned()],
    );
}
