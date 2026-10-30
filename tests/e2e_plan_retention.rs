//! e2e suite: plan retention: live plans pin archives, commits sweep expired plans, and bound runs hash local archives again.
#![cfg(test)]

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use common::archives::{RawEntry, raw_zip};
use common::home::Home;
use common::http::{Asset, gh_release};
use common::netlog::net_log;
use common::plans::{lease_files, plan_files, plan_id, plans_dir, plant_expired, read_plan};
use common::state::{history, record};
use common::tlog::{Ran, TestLog, tlog};
use jiff::{SignedDuration, Timestamp};
use omaddons::model::PlanId;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use ulid::Ulid;

const NOW: &str = "2026-10-28T12:00:00Z";
const FLAVORS: &[(&str, u32)] = &[
    ("mainline", 120_105),
    ("classic", 11_509),
    ("mists", 50_504),
];

fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
fn time(hours: i64) -> String {
    NOW.parse::<Timestamp>()
        .unwrap()
        .checked_add(SignedDuration::from_hours(hours))
        .unwrap()
        .to_string()
}

fn release(home: &Home, name: &str, tag: &str) {
    let file = format!("{name}.zip");
    let toc_path = format!("{name}/{name}.toc");
    let lua_path = format!("{name}/core.lua");
    let toc = common::addons::toc(
        "11509, 50504, 120105",
        &[&format!("## Title: {name}"), &format!("## Version: {tag}")],
    );
    let lua = format!("-- {name} {tag}");
    let assets: [Asset<'_>; 1] = [(&file, FLAVORS, vec![(&toc_path, &toc), (&lua_path, &lua)])];
    gh_release(
        &home.http,
        &format!("https://github.com/o/{}", name.to_ascii_lowercase()),
        tag,
        &assets,
    );
}

fn run(log: &TestLog, home: &Home, args: &[&str], clock: &str) -> Ran {
    let mut argv: Vec<String> = args.iter().map(|arg| (*arg).into()).collect();
    argv.extend(["--json".into(), "--progress".into(), "json".into()]);
    log.cmd(home, &argv).env("OMADDONS_NOW", clock).run()
}

fn blobs(home: &Home) -> BTreeMap<String, u64> {
    let dir = home.env.cache().join("omaddons/blobs");
    match fs::read_dir(dir) {
        Ok(entries) => entries
            .map(|entry| entry.unwrap())
            .filter_map(|entry| {
                let name = entry.file_name().into_string().unwrap();
                (name.len() == 64
                    && name
                        .bytes()
                        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()))
                .then(|| (name, entry.metadata().unwrap().len()))
            })
            .collect(),
        Err(error) => {
            assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
            BTreeMap::new()
        }
    }
}

fn state(log: &TestLog, home: &Home) {
    log.json("blobs", &blobs(home));
    let plans: Vec<_> = plan_files(&home.env)
        .iter()
        .map(|path| {
            let bytes = fs::read(path).unwrap();
            let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
            json!({
                "id": path.file_stem().unwrap().to_str().unwrap(),
                "created_at": value["created_at"],
                "expires_at": value["expires_at"],
                "bytes": bytes.len(),
                "sha256": digest(&bytes),
            })
        })
        .collect();
    let leases: Vec<_> = lease_files(&home.env)
        .iter()
        .map(|path| path.file_name().unwrap().to_str().unwrap().to_owned())
        .collect();
    log.json("store", &json!({"plans":plans,"leases":leases}));
}

fn commit(log: &TestLog, home: &Home, args: &[&str], clock: &str) -> Ran {
    state(log, home);
    let ran = run(log, home, args, clock);
    ran.ok().assert_private();
    log.assert_eq(
        "retention never failed the commit",
        &ran.warnings()
            .iter()
            .any(|warning| warning == "retention_failed"),
        &false,
    );
    state(log, home);
    ran
}

fn home(log: &TestLog, cap: Option<u64>) -> Home {
    log.step("setup: install A v1 on retail");
    let home = Home::new(&["wow", "wow_classic_era"]);
    if let Some(cap) = cap {
        let config = home.env.config().join("omaddons");
        fs::create_dir_all(&config).unwrap();
        fs::write(
            config.join("config.toml"),
            format!("[limits]\nblob_cache_max_bytes = {cap}\n"),
        )
        .unwrap();
    }
    release(&home, "A", "v1");
    commit(log, &home, &["install", "gh:o/a", "-t", "retail"], NOW);
    release(&home, "A", "v2");
    for name in ["B", "C", "D"] {
        release(&home, name, "v1");
    }
    home
}

fn preview(log: &TestLog, home: &Home, clock: &str) -> (String, String, String) {
    log.step("preview");
    let ran = run(
        log,
        home,
        &["update", "-t", "retail", "--refresh", "--dry-run"],
        clock,
    );
    ran.ok();
    let id = ran.data()["plan_id"].as_str().unwrap().to_owned();
    id.parse::<PlanId>().unwrap();
    let token = ran.data()["precondition"].as_str().unwrap().to_owned();
    let plan = read_plan(&home.env, &id);
    log.json("plan", &plan);
    let blob = plan["releases"][0]["blob_sha256"]
        .as_str()
        .unwrap()
        .to_owned();
    state(log, home);
    (id, token, blob)
}

fn plant_blob(home: &Home, bytes: &[u8]) -> String {
    let hash = digest(bytes);
    let dir = home.env.cache().join("omaddons/blobs");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(&hash), bytes).unwrap();
    hash
}

/// The archive of the one addon installed on `target`: record-pinned.
fn installed_blob(home: &Home, target: &str) -> String {
    record(home, target)
        .addons
        .values()
        .next()
        .unwrap()
        .blob_sha256
        .clone()
        .unwrap()
}

#[test]
fn an_unexpired_plans_archive_survives_eviction_and_is_not_downloaded_again() {
    let log = tlog!();
    let home = home(&log, Some(1));
    let (id, token, s2) = preview(&log, &home, NOW);
    let a1 = installed_blob(&home, "retail");
    log.assert_eq(
        "preview archive cached",
        &blobs(&home).contains_key(&s2),
        &true,
    );
    log.step("plant an unpinned archive");
    let x = plant_blob(&home, &[42; 4096]);
    log.step("commit on era");
    commit(&log, &home, &["install", "gh:o/b", "-t", "era"], NOW);
    let b1 = installed_blob(&home, "era");
    let kept = blobs(&home);
    log.assert_eq("unpinned archive evicted", &kept.contains_key(&x), &false);
    log.assert_eq(
        "plan and record archives kept",
        &(kept.contains_key(&s2) && kept.contains_key(&a1) && kept.contains_key(&b1)),
        &true,
    );
    log.assert_eq(
        "live plan kept",
        &plans_dir(&home.env).join(format!("{id}.json")).exists(),
        &true,
    );
    log.step("bound run");
    let assets = net_log(&home).count_purpose("asset");
    commit(
        &log,
        &home,
        &["update", "-t", "retail", "--refresh", "--if-match", &token],
        NOW,
    );
    log.assert_eq(
        "bound update reused the archive",
        &net_log(&home).count_purpose("asset"),
        &assets,
    );
    log.assert_eq(
        "A updated",
        &record(&home, "retail")
            .addons
            .values()
            .next()
            .unwrap()
            .version,
        &"v2".to_owned(),
    );
    log.step("the old archive is free");
    let kept = blobs(&home);
    log.assert_eq("old archive evicted", &kept.contains_key(&a1), &false);
    log.assert_eq(
        "only A v2 and B remain",
        &kept.into_keys().collect::<BTreeSet<_>>(),
        &BTreeSet::from([s2, b1]),
    );
}

#[test]
fn an_expired_plans_archive_is_evicted_and_the_plan_swept_after_a_commit() {
    let log = tlog!();
    let home = home(&log, Some(1));
    let (id, _token, s2) = preview(&log, &home, NOW);
    let a1 = installed_blob(&home, "retail");
    log.step("commit after expiry");
    let later = time(25);
    commit(&log, &home, &["install", "gh:o/b", "-t", "era"], &later);
    let kept = blobs(&home);
    log.assert_eq("expired archive evicted", &kept.contains_key(&s2), &false);
    log.assert_eq(
        "record archives kept",
        &(kept.contains_key(&a1) && kept.contains_key(&installed_blob(&home, "era"))),
        &true,
    );
    log.assert_eq(
        "expired plan swept",
        &plans_dir(&home.env).join(format!("{id}.json")).exists(),
        &false,
    );
    log.step("the pin only saved a download");
    let assets = net_log(&home).count_purpose("asset");
    preview(&log, &home, &later);
    log.assert_eq(
        "new preview fetched one archive",
        &net_log(&home).count_purpose("asset"),
        &(assets + 1),
    );
}

#[test]
fn retention_sweeps_expired_plans_in_bounded_batches_until_none_is_left() {
    let log = tlog!();
    let home = home(&log, None);
    let (id, _, _) = preview(&log, &home, NOW);
    let template = read_plan(&home.env, &id);
    let original = fs::read(plans_dir(&home.env).join(format!("{id}.json"))).unwrap();
    log.step("plant");
    plant_expired(&home.env, &template, 150);
    let expired: Vec<_> = plan_files(&home.env)
        .into_iter()
        // ubs:ignore — comparing plan ids (file names), not secrets
        .filter(|p| p.file_stem().unwrap() != id.as_str())
        .collect();
    for path in &expired[..10] {
        fs::write(path.with_extension("lease"), b"").unwrap();
    }
    let lease = fs::File::options()
        .read(true)
        .write(true)
        .open(expired[5].with_extension("lease"))
        .unwrap();
    lease.lock().unwrap();
    let orphan = plans_dir(&home.env).join(format!("{}.lease", Ulid::generate()));
    fs::write(&orphan, b"").unwrap();
    for (name, remaining, leases) in [("b", 87, 55), ("c", 24, 64)] {
        log.step(&format!("commit {name}"));
        commit(
            &log,
            &home,
            &["install", &format!("gh:o/{name}"), "-t", "era"],
            NOW,
        );
        log.assert_eq(
            "expired plans remaining",
            &(plan_files(&home.env).len() - 1),
            &remaining,
        );
        log.assert_eq("held plan remains", &expired[5].exists(), &true);
        log.assert_eq(
            "held lease remains",
            &expired[5].with_extension("lease").exists(),
            &true,
        );
        log.assert_eq("orphan lease removed", &orphan.exists(), &false);
        // Prune creates a guard for a missing lease. Such a new orphan was not in
        // this pass's listing; the following commit sweeps it.
        log.assert_eq(
            "held and newly orphaned leases",
            &lease_files(&home.env).len(),
            &leases,
        );
        log.assert_eq(
            "initial free leases removed",
            &expired[..10]
                .iter()
                .enumerate()
                .all(|(i, path)| i == 5 || !path.with_extension("lease").exists()),
            &true,
        );
    }
    drop(lease);
    log.step("release and commit 3");
    commit(&log, &home, &["install", "gh:o/d", "-t", "era"], NOW);
    log.assert_eq(
        "only the live plan remains",
        &plan_files(&home.env).len(),
        &1,
    );
    log.assert_eq(
        "new guard leases await the next pass",
        &lease_files(&home.env).len(),
        &23,
    );
    log.step("commit 4 collects the last free guard leases");
    commit(&log, &home, &["pin", "B", "-t", "era"], NOW);
    log.assert_eq("no leases remain", &lease_files(&home.env).len(), &0);
    log.assert_eq(
        "live plan unchanged",
        &fs::read(plans_dir(&home.env).join(format!("{id}.json"))).unwrap(),
        &original,
    );
}

#[test]
fn retention_skips_the_sweep_while_a_preview_holds_the_admission_lock() {
    let log = tlog!();
    let home = home(&log, None);
    let (id, _, _) = preview(&log, &home, NOW);
    let template = read_plan(&home.env, &id);
    log.step("store");
    plant_expired(&home.env, &template, 5);
    log.tree("plans", &plans_dir(&home.env));
    let lock = fs::File::options()
        .read(true)
        .write(true)
        .open(plans_dir(&home.env).join(".lock"))
        .unwrap();
    lock.lock().unwrap();
    log.step("admission busy");
    let ran = commit(&log, &home, &["install", "gh:o/b", "-t", "era"], NOW);
    log.assert_eq(
        "admission timeout was not waited",
        &(ran.elapsed_ms < 5000),
        &true,
    );
    log.assert_unchanged("plans", &plans_dir(&home.env));
    drop(lock);
    log.step("admission free");
    commit(&log, &home, &["install", "gh:o/c", "-t", "era"], NOW);
    log.assert_eq("expired plans swept", &plan_files(&home.env).len(), &1);
    log.step("no store, none created");
    let fresh = home_without_preview(&log);
    commit(&log, &fresh, &["install", "gh:o/b", "-t", "era"], NOW);
    log.assert_eq(
        "retention created no plan directory",
        &plans_dir(&fresh.env).exists(),
        &false,
    );
}

/// `home`, callable where a local `home` shadows it.
fn home_without_preview(log: &TestLog) -> Home {
    home(log, None)
}

#[test]
fn unreadable_and_oversized_plans_pin_nothing_and_fail_no_commit() {
    let log = tlog!();
    let home = home(&log, Some(1));
    let (id, _, s2) = preview(&log, &home, NOW);
    let template = read_plan(&home.env, &id);
    let y = plant_blob(&home, &[17; 4096]);
    let z = plant_blob(&home, &[18; 4096]);
    log.step("plant bad plans");
    let mut bad = Vec::new();
    for case in ["oversized", "inconsistent", "garbage"] {
        let id = Ulid::generate().to_string();
        let mut value = template.clone();
        value["plan_id"] = Value::from(id.clone());
        let bytes = match case {
            "oversized" => {
                value["releases"][0]["blob_sha256"] = Value::from(y.clone());
                value["summary"] =
                    json!(["x".repeat(usize::try_from(omaddons::limits::PLAN_MAX_BYTES).unwrap())]);
                serde_json::to_vec(&value).unwrap()
            }
            "inconsistent" => {
                value["releases"][0]["blob_sha256"] = Value::from(z.clone());
                value["precondition"]["token"] = Value::from(format!("pc1:{}", "0".repeat(64)));
                serde_json::to_vec(&value).unwrap()
            }
            _ => b"{\"schema_version\":\"1.0\",\"plan_id\":\"".to_vec(),
        };
        let path = plans_dir(&home.env).join(format!("{id}.json"));
        fs::write(&path, &bytes).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        bad.push((path, bytes));
    }
    log.step("commit");
    commit(&log, &home, &["install", "gh:o/b", "-t", "era"], NOW);
    let kept = blobs(&home);
    log.assert_eq("good plan still pins", &kept.contains_key(&s2), &true);
    log.assert_eq(
        "good plan kept",
        &plans_dir(&home.env).join(format!("{id}.json")).exists(),
        &true,
    );
    log.assert_eq(
        "bad plans pin no archives",
        &(!kept.contains_key(&y) && !kept.contains_key(&z)),
        &true,
    );
    for (path, bytes) in bad {
        log.assert_eq(
            "bad file unchanged",
            &digest(&fs::read(path).unwrap()),
            &digest(&bytes),
        );
    }
}

fn local_zip(home: &Home, code: &[u8]) -> PathBuf {
    let dir = home.env.root().join("dl");
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("foo.zip");
    fs::write(
        &path,
        raw_zip(&[
            RawEntry::stored("Foo/Foo.toc", b"## Interface: 120105\n"),
            RawEntry::stored("Foo/core.lua", code),
        ]),
    )
    .unwrap();
    path
}
fn local_args<'a>(zip: &'a Path, extra: &'a str, value: Option<&'a str>) -> Vec<&'a str> {
    let mut args = vec![
        "install",
        "gh:o/foo",
        "--file",
        zip.to_str().unwrap(),
        "-t",
        "retail",
        extra,
    ];
    if let Some(value) = value {
        args.push(value);
    }
    args
}
fn zip_fact(path: &Path) -> Value {
    let meta = fs::metadata(path).unwrap();
    let mtime = meta
        .modified()
        .unwrap()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap();
    json!({
        "sha256": digest(&fs::read(path).unwrap()),
        "bytes": meta.len(),
        "mtime": mtime.as_nanos().to_string(),
    })
}

#[test]
fn a_local_zip_edited_after_its_preview_stales_the_bound_run() {
    let log = tlog!();
    let home = Home::new(&["wow"]);
    let zip = local_zip(&home, b"-- a");
    log.step("preview");
    let ran = run(&log, &home, &local_args(&zip, "--dry-run", None), NOW);
    ran.ok();
    let token = ran.data()["precondition"].as_str().unwrap().to_owned();
    log.tree("addons", &home.addons("_retail_"));
    let before = zip_fact(&zip);
    log.json("zip before", &before);
    let old_mtime = fs::metadata(&zip).unwrap().modified().unwrap();
    let history_before = history(&home).len();
    log.step("edit, same size, same mtime");
    local_zip(&home, b"-- b");
    fs::File::options()
        .write(true)
        .open(&zip)
        .unwrap()
        .set_modified(old_mtime)
        .unwrap();
    let after = zip_fact(&zip);
    log.json("zip after", &after);
    log.assert_eq("same length", &after["bytes"], &before["bytes"]);
    log.assert_eq("same mtime", &after["mtime"], &before["mtime"]);
    log.assert_eq(
        "different digest",
        &(after["sha256"] != before["sha256"]),
        &true,
    );
    log.step("bound run");
    let ran = run(
        &log,
        &home,
        &local_args(&zip, "--if-match", Some(&token)),
        NOW,
    );
    ran.fails(1, "plan_stale").assert_private();
    log.assert_eq(
        "bound refusal is not retryable",
        &ran.envelope()["error"]["retryable"],
        &json!(false),
    );
    log.assert_eq(
        "selection and observation moved",
        &ran.envelope()["error"]["details"]["changed"],
        &json!(["selection", "observed"]),
    );
    log.assert_unchanged("addons", &home.addons("_retail_"));
    log.assert_eq("history unchanged", &history(&home).len(), &history_before);
    log.step("re-preview");
    let ran = run(&log, &home, &local_args(&zip, "--dry-run", None), NOW);
    ran.ok();
    let fresh = ran.data()["precondition"].as_str().unwrap();
    // A public precondition digest, compared for content rather than authentication.
    log.assert_eq("new token", &fresh.eq(token.as_str()), &false);
    commit(
        &log,
        &home,
        &local_args(&zip, "--if-match", Some(fresh)),
        NOW,
    );
    log.assert_eq(
        "new bytes installed",
        &fs::read(home.addons("_retail_").join("Foo/core.lua")).unwrap(),
        &b"-- b".to_vec(),
    );
}

#[test]
fn a_local_zip_only_touched_after_its_preview_still_applies() {
    let log = tlog!();
    let home = Home::new(&["wow"]);
    let zip = local_zip(&home, b"-- a");
    log.step("preview");
    let ran = run(&log, &home, &local_args(&zip, "--dry-run", None), NOW);
    ran.ok();
    let token = ran.data()["precondition"].as_str().unwrap();
    let before = zip_fact(&zip);
    log.json("zip before", &before);
    log.step("touch only");
    let mtime = fs::metadata(&zip).unwrap().modified().unwrap() + Duration::from_secs(120);
    fs::File::options()
        .write(true)
        .open(&zip)
        .unwrap()
        .set_modified(mtime)
        .unwrap();
    let after = zip_fact(&zip);
    log.json("zip after", &after);
    log.assert_eq("digest unchanged", &after["sha256"], &before["sha256"]);
    // ubs:ignore — comparing modification times, not secrets
    log.assert_eq("mtime changed", &(after["mtime"] != before["mtime"]), &true);
    commit(
        &log,
        &home,
        &local_args(&zip, "--if-match", Some(token)),
        NOW,
    );
    log.assert_eq(
        "original bytes installed",
        &fs::read(home.addons("_retail_").join("Foo/core.lua")).unwrap(),
        &b"-- a".to_vec(),
    );
}

// ---------------------------------------------------------------------------------------
// The same proofs through `apply <plan_id>`, which runs the stored intent bound to its
// stored token.

/// Each `apply` outcome's target and status, in order.
fn outcomes(ran: &Ran) -> Vec<(String, String)> {
    ran.data()["outcomes"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|o| {
            let text = |key: &str| o[key].as_str().unwrap_or_default().to_owned();
            (text("target"), text("status"))
        })
        .collect()
}

/// A local-zip install previewed: the plan it stored.
fn preview_local(log: &TestLog, home: &Home, zip: &Path) -> String {
    log.step("preview");
    let ran = run(log, home, &local_args(zip, "--dry-run", None), NOW);
    ran.ok();
    let id = plan_id(&ran);
    log.json("plan id", &id);
    id
}

/// A live plan pins the archive it would install: a commit elsewhere evicts what no plan
/// or record pins, and `apply` then installs from the pinned archive with no download.
#[test]
fn apply_uses_a_pinned_archive_after_an_eviction() {
    let log = tlog!();
    let home = home(&log, Some(1));
    let (id, _token, s2) = preview(&log, &home, NOW);
    let a1 = installed_blob(&home, "retail");
    log.json("archives: A v1, A v2", &json!({"a1": a1, "s2": s2}));
    log.assert_eq(
        "preview archive cached",
        &blobs(&home).contains_key(&s2),
        &true,
    );
    log.step("plant an unpinned archive");
    let x = plant_blob(&home, &[42; 4096]);
    log.step("commit on era");
    commit(&log, &home, &["install", "gh:o/b", "-t", "era"], NOW);
    let b1 = installed_blob(&home, "era");
    let kept = blobs(&home);
    log.json("archives after the eviction", &kept);
    log.assert_eq("unpinned archive evicted", &kept.contains_key(&x), &false);
    log.assert_eq(
        "plan and record archives kept",
        &(kept.contains_key(&s2) && kept.contains_key(&a1) && kept.contains_key(&b1)),
        &true,
    );

    log.step("apply the stored plan");
    let assets = net_log(&home).count_purpose("asset");
    log.json("asset requests before apply", &assets);
    let ran = commit(&log, &home, &["apply", &id], NOW);
    log.assert_eq(
        "the update committed on retail",
        &outcomes(&ran),
        &vec![("retail".to_owned(), "committed".to_owned())],
    );
    log.assert_eq(
        "apply downloaded no archive",
        &net_log(&home).count_purpose("asset"),
        &assets,
    );
    log.assert_eq(
        "A updated",
        &record(&home, "retail")
            .addons
            .values()
            .next()
            .unwrap()
            .version,
        &"v2".to_owned(),
    );

    log.step("the old archive is free");
    let kept = blobs(&home);
    log.json("archives after apply", &kept);
    log.assert_eq("old archive evicted", &kept.contains_key(&a1), &false);
    log.assert_eq(
        "only A v2 and B remain",
        &kept.into_keys().collect::<BTreeSet<_>>(),
        &BTreeSet::from([s2, b1]),
    );
}

/// `apply` hashes a plan's local archive again: other bytes at the same size and time
/// are another selection, refused with nothing changed, while a touch alone applies.
#[test]
fn apply_hashes_a_local_zip_again() {
    let log = tlog!();
    {
        let log = log.case("edited");
        let home = Home::new(&["wow"]);
        let zip = local_zip(&home, b"-- a");
        let id = preview_local(&log, &home, &zip);
        log.tree("addons", &home.addons("_retail_"));
        let before = zip_fact(&zip);
        log.json("zip before", &before);
        let old_mtime = fs::metadata(&zip).unwrap().modified().unwrap();
        let history_before = history(&home).len();

        log.step("edit, same size, same mtime");
        local_zip(&home, b"-- b");
        fs::File::options()
            .write(true)
            .open(&zip)
            .unwrap()
            .set_modified(old_mtime)
            .unwrap();
        let after = zip_fact(&zip);
        log.json("zip after", &after);
        log.assert_eq("same length", &after["bytes"], &before["bytes"]);
        log.assert_eq("same mtime", &after["mtime"], &before["mtime"]);
        log.assert_eq(
            "different digest",
            &(after["sha256"] != before["sha256"]),
            &true,
        );

        log.step("apply the plan of the original bytes");
        let ran = run(&log, &home, &["apply", &id], NOW);
        ran.fails(1, "plan_stale").assert_private();
        let error = &ran.envelope()["error"];
        log.json("details", &error["details"]);
        log.assert_eq("not retryable", &error["retryable"], &json!(false));
        let changed = error["details"]["changed"].as_array().cloned().unwrap();
        log.assert_eq(
            "the selection moved",
            &changed.contains(&json!("selection")),
            &true,
        );
        log.assert_unchanged("addons", &home.addons("_retail_"));
        log.assert_eq("history unchanged", &history(&home).len(), &history_before);

        log.step("re-preview the edited archive and apply it");
        let fresh = preview_local(&log, &home, &zip);
        // ubs:ignore — comparing plan ids, not secrets
        log.assert_eq("a new plan", &(fresh != id), &true);
        let ran = commit(&log, &home, &["apply", &fresh], NOW);
        log.assert_eq(
            "committed on retail",
            &outcomes(&ran),
            &vec![("retail".to_owned(), "committed".to_owned())],
        );
        log.assert_eq(
            "new bytes installed",
            &fs::read(home.addons("_retail_").join("Foo/core.lua")).unwrap(),
            &b"-- b".to_vec(),
        );
    }
    {
        let log = log.case("touched");
        let home = Home::new(&["wow"]);
        let zip = local_zip(&home, b"-- a");
        let id = preview_local(&log, &home, &zip);
        let before = zip_fact(&zip);
        log.json("zip before", &before);

        log.step("touch only");
        let mtime = fs::metadata(&zip).unwrap().modified().unwrap() + Duration::from_secs(120);
        fs::File::options()
            .write(true)
            .open(&zip)
            .unwrap()
            .set_modified(mtime)
            .unwrap();
        let after = zip_fact(&zip);
        log.json("zip after", &after);
        log.assert_eq("digest unchanged", &after["sha256"], &before["sha256"]);
        // ubs:ignore — comparing modification times, not secrets
        log.assert_eq("mtime changed", &(after["mtime"] != before["mtime"]), &true);

        log.step("apply");
        let ran = commit(&log, &home, &["apply", &id], NOW);
        log.assert_eq(
            "committed on retail",
            &outcomes(&ran),
            &vec![("retail".to_owned(), "committed".to_owned())],
        );
        log.assert_eq(
            "original bytes installed",
            &fs::read(home.addons("_retail_").join("Foo/core.lua")).unwrap(),
            &b"-- a".to_vec(),
        );
    }
}

/// A plan whose local archive is gone is refused before anything runs (`input_missing`),
/// and applies once the archive is back and previewed again.
#[test]
fn apply_refuses_a_deleted_local_input() {
    let log = tlog!();
    let home = Home::new(&["wow"]);
    let zip = local_zip(&home, b"-- a");
    let id = preview_local(&log, &home, &zip);
    let path = zip.canonicalize().unwrap().display().to_string();
    log.json("zip", &zip_fact(&zip));
    log.tree("addons", &home.addons("_retail_"));
    let history_before = history(&home).len();

    log.step("delete the archive");
    fs::remove_file(&zip).unwrap();

    log.step("apply");
    let ran = run(&log, &home, &["apply", &id], NOW);
    ran.fails(1, "plan_stale").assert_private();
    let details = &ran.envelope()["error"]["details"];
    log.json("details", details);
    log.assert_eq("input missing", &details["reason"], &json!("input_missing"));
    log.assert_eq("the archive's path", &details["path"], &json!(path));
    log.assert_unchanged("addons", &home.addons("_retail_"));
    log.assert_eq("history unchanged", &history(&home).len(), &history_before);

    log.step("restore the archive");
    local_zip(&home, b"-- a");
    log.json("zip restored", &zip_fact(&zip));
    let again = preview_local(&log, &home, &zip);
    log.json(
        "the same intent and token reuse the plan",
        &json!({"first": id, "again": again}),
    );
    let ran = commit(&log, &home, &["apply", &again], NOW);
    log.assert_eq(
        "committed on retail",
        &outcomes(&ran),
        &vec![("retail".to_owned(), "committed".to_owned())],
    );
    log.assert_eq(
        "installed",
        &fs::read(home.addons("_retail_").join("Foo/core.lua")).unwrap(),
        &b"-- a".to_vec(),
    );
}
