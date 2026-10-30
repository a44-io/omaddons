//! Release landings between a preview and `apply` (D7, M4-D20): for every source, a
//! release published after an `update` preview makes `apply` stale (`selection`), never a
//! different install, and the re-preview shows it from the memo the stale `apply`
//! refreshed; the same for `install`, `sync` and `copy` on GitHub, beside the typed
//! `--if-match` runs that keep their command's freshness; and a Tukui plan applied from
//! the cache its preview filled. Every run is on the real clock: the landings' memo
//! carries real times.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::common::home::Home;
use crate::common::http::paths;
use crate::common::landing::{Landing, Source};
use crate::common::memo;
use crate::common::netlog::net_log;
use crate::common::plans::{plan_files, plan_id, read_plan};
use crate::common::state::{key_of, record};
use crate::common::tlog::{Ran, TestLog, tlog};

// ---------------------------------------------------------------------------------------
// Support.

/// `omaddons <args> --json`.
fn run(log: &TestLog, home: &Home, args: &[&str]) -> Ran {
    let mut argv = args.to_vec();
    argv.push("--json");
    log.run(home, &argv)
}

/// [`run`], which must succeed.
#[track_caller]
fn ok(log: &TestLog, home: &Home, args: &[&str]) -> Ran {
    let ran = run(log, home, args);
    ran.ok();
    ran
}

/// Serves `source`'s release 1 in a fresh scenario, and logs it.
fn start(log: &TestLog, home: &mut Home, source: Source) -> Landing {
    let landing = Landing::start(home, source);
    landed(log, home, &landing);
    landing
}

/// Serves the landing's next release, and logs it; returns its version.
fn land(log: &TestLog, home: &mut Home, landing: &mut Landing) -> String {
    let version = landing.land(home);
    landed(log, home, landing);
    version
}

fn landed(log: &TestLog, home: &Home, landing: &Landing) {
    let n = landing.newest();
    log.note(format!(
        "landed {} release {n}: {}",
        landing.source().label(),
        landing.version(n)
    ));
    let mut files = Vec::new();
    fixture_files(&home.http, &home.http, &mut files);
    files.sort();
    log.json("scenario", &files);
}

/// The fixture files (`*.json`) under `dir`, relative to `root`.
fn fixture_files(root: &Path, dir: &Path, out: &mut Vec<String>) {
    for item in fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = item.path();
        if path.is_dir() {
            fixture_files(root, &path, out);
        } else if path.extension().is_some_and(|ext| ext == "json") {
            out.push(path.strip_prefix(root).unwrap().display().to_string());
        }
    }
}

/// `net.log`'s requests so far, counted by purpose.
fn requests(home: &Home) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for entry in net_log(home).entries {
        *counts.entry(entry.purpose).or_default() += 1;
    }
    counts
}

/// The requests added since `before`, by purpose; logged.
fn added(log: &TestLog, home: &Home, before: &BTreeMap<String, usize>) -> BTreeMap<String, usize> {
    let added: BTreeMap<String, usize> = requests(home)
        .into_iter()
        .filter_map(|(purpose, n)| {
            let more = n - before.get(&purpose).copied().unwrap_or(0);
            (more > 0).then_some((purpose, more))
        })
        .collect();
    log.note(format!("requests added, by purpose: {added:?}"));
    added
}

fn count(requests: &BTreeMap<String, usize>, purpose: &str) -> usize {
    requests.get(purpose).copied().unwrap_or(0)
}

/// The `to_version` of every item a run reports, whether its data holds one target's
/// `items` or `targets[].items`.
fn to_versions(ran: &Ran) -> Vec<String> {
    let data = ran.data();
    let items: Vec<&Value> = match data["items"].as_array() {
        Some(items) => items.iter().collect(),
        None => data["targets"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|t| t["items"].as_array().into_iter().flatten())
            .collect(),
    };
    items
        .iter()
        .filter_map(|item| item["to_version"].as_str().map(str::to_owned))
        .collect()
}

fn words(value: &Value) -> Vec<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|word| word.as_str().map(str::to_owned))
        .collect()
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Each plan file's name and digest.
fn plan_digests(home: &Home) -> Vec<(String, String)> {
    plan_files(&home.env)
        .iter()
        .map(|path| {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            (name, sha256(&fs::read(path).unwrap()))
        })
        .collect()
}

fn history_lines(home: &Home) -> usize {
    fs::read_to_string(paths(&home.env).history_file())
        .map(|text| text.lines().count())
        .unwrap_or(0)
}

/// What a refused run leaves as it was: each client's AddOns, the records, the plan
/// files and history's length.
struct Trees {
    roots: Vec<(&'static str, PathBuf)>,
    plans: Vec<(String, String)>,
    history: usize,
}

impl Trees {
    fn record(log: &TestLog, home: &Home) -> Self {
        let mut roots = vec![("addons", home.addons("_retail_"))];
        if home.client("_classic_").exists() {
            roots.push(("addons classic", home.addons("_classic_")));
        }
        roots.push(("records", home.state().join("targets")));
        for (label, root) in &roots {
            log.tree(label, root);
        }
        let trees = Self {
            roots,
            plans: plan_digests(home),
            history: history_lines(home),
        };
        log.json("plan files", &trees.plans);
        log.json("history lines", &trees.history);
        trees
    }

    fn unchanged(&self, log: &TestLog, home: &Home) {
        for (label, root) in &self.roots {
            log.assert_unchanged(label, root);
        }
        log.assert_eq("plan files", &plan_digests(home), &self.plans);
        log.assert_eq("history lines", &history_lines(home), &self.history);
    }
}

/// `apply`'s refusal of a plan a release landing made stale: `plan_stale`, not
/// retryable, `selection` alone and no lifecycle reason, its fix the plan's preview.
#[track_caller]
fn stale_selection(log: &TestLog, ran: &Ran, plan: &Value) {
    ran.fails(1, "plan_stale");
    let error = &ran.envelope()["error"];
    log.json("refusal", error);
    log.assert_eq(
        "changed",
        &error["details"]["changed"],
        &json!(["selection"]),
    );
    log.assert_eq("not retryable", &error["retryable"], &json!(false));
    log.assert_eq(
        "no lifecycle reason",
        &error["details"]
            .get("reason")
            .cloned()
            .unwrap_or(Value::Null),
        &Value::Null,
    );
    let preview = omaddons::app::precondition::preview_argv(&words(&plan["display_argv"]));
    log.assert_eq(
        "the fix previews the plan again",
        &error["fix_argv"],
        &json!(preview),
    );
}

/// A typed `--if-match` run the landing made stale: `plan_stale`, `selection` alone.
#[track_caller]
fn typed_stale(log: &TestLog, ran: &Ran) {
    ran.fails(1, "plan_stale");
    let error = &ran.envelope()["error"];
    log.json("refusal", error);
    log.assert_eq(
        "changed",
        &error["details"]["changed"],
        &json!(["selection"]),
    );
}

/// `marker` on `folder`'s AddOns: the file holds its content.
#[track_caller]
fn has_marker(log: &TestLog, home: &Home, folder: &str, marker: Option<(String, String)>) {
    if let Some((path, content)) = marker {
        let found = fs::read_to_string(home.addons(folder).join(&path)).unwrap_or_default();
        log.assert_eq(&format!("marker {path} on {folder}"), &found, &content);
    }
}

/// A plan stored by the dry run `ran`: its id and file.
fn stored(log: &TestLog, home: &Home, ran: &Ran) -> (String, Value) {
    let id = plan_id(ran);
    let plan = read_plan(&home.env, &id);
    log.json("plan file", &plan);
    (id, plan)
}

/// The plan id history's newest line carries.
fn newest_plan(log: &TestLog, home: &Home) -> Value {
    let history = ok(log, home, &["history", "--limit", "1"]);
    history.data()["entries"][0]["plan"].clone()
}

/// An `apply` that committed on retail alone: its one outcome.
#[track_caller]
fn committed_on_retail(log: &TestLog, home: &Home, ran: &Ran) {
    let outcomes = ran.data()["outcomes"].clone();
    log.json("outcomes", &outcomes);
    let tx = outcomes[0]["tx"].clone();
    log.assert_eq("a transaction", &tx.is_string(), &true);
    log.assert_eq(
        "outcomes",
        &outcomes,
        &json!([{
            "target": "retail",
            "key": key_of(home, "retail").to_string(),
            "status": "committed",
            "tx": tx,
            "error_code": null,
        }]),
    );
}

// ---------------------------------------------------------------------------------------
// Per source: the landing builders' self-test, and the stale `update` plan.

macro_rules! per_source {
    ($scenario:ident: $($test:ident => $source:ident),+ $(,)?) => {
        $(
            #[test]
            fn $test() {
                let log = tlog!();
                $scenario(&log, Source::$source);
            }
        )+
    };
}

per_source!(landing:
    drift_landing_github => GitHub,
    drift_landing_git => Git,
    drift_landing_wowi => Wowi,
    drift_landing_tukui => Tukui,
    drift_landing_wowace => WowAce,
);

per_source!(update:
    drift_update_github => GitHub,
    drift_update_git => Git,
    drift_update_wowi => Wowi,
    drift_update_tukui => Tukui,
    drift_update_wowace => WowAce,
);

/// Release 1 installs; after each landing and a day's wait, `outdated` reports the next
/// release through the source's own revalidation request, downloading nothing.
fn landing(log: &TestLog, source: Source) {
    let label = source.label();
    let mut home = Home::new(&["wow"]);
    log.env(&home);

    log.step("release 1");
    let mut landing = start(log, &mut home, source);
    let installed = ok(log, &home, &["install", landing.r#ref(), "-t", "retail"]);
    log.assert_eq(
        &format!("{label}: installs release 1"),
        &to_versions(&installed),
        &vec![landing.version(1)],
    );
    has_marker(log, &home, "_retail_", landing.marker(1));

    for n in 2..=3 {
        log.step(&format!("release {n} lands"));
        let version = land(log, &mut home, &mut landing);
        log.assert_eq(
            &format!("{label}: release {n}'s version"),
            &version,
            &landing.version(n),
        );
        memo::age(&home, 24);
        let before = requests(&home);
        let outdated = ok(log, &home, &["outdated", "-t", "retail"]);
        let items = outdated.data()["targets"][0]["items"].clone();
        log.json("outdated items", &items);
        log.assert_eq(
            &format!("{label}: one addon is behind"),
            &items.as_array().map(Vec::len),
            &Some(1),
        );
        let item = &items[0];
        log.assert_eq(
            &format!("{label}: release {n} available"),
            &(&item["status"], &item["latest"]["version"]),
            &(&json!("update_available"), &json!(landing.version(n))),
        );
        if let Some(revision) = landing.revision(n) {
            log.assert_eq(
                &format!("{label}: at revision {revision}"),
                &item["latest"]["revision"],
                &json!(revision),
            );
        }
        let cost = added(log, &home, &before);
        log.assert_eq(
            &format!("{label}: asked {}", landing.probe()),
            &(count(&cost, landing.probe()) > 0),
            &true,
        );
        log.assert_eq(
            &format!("{label}: downloaded nothing"),
            &count(&cost, "asset"),
            &0,
        );
    }
}

/// A release landing between an `update` preview and its `apply`: the apply is stale,
/// the re-preview inside the window shows the release from the memo the stale apply
/// refreshed, and that plan applies exactly, after one revalidation request.
fn update(log: &TestLog, source: Source) {
    let label = source.label();
    let mut home = Home::new(&["wow"]);
    log.env(&home);

    log.step("install release 1");
    let mut landing = start(log, &mut home, source);
    ok(log, &home, &["install", landing.r#ref(), "-t", "retail"]);

    log.step("release 2 lands, a day passes");
    land(log, &mut home, &mut landing);
    memo::age(&home, 24);

    log.step("preview");
    let previewed = ok(log, &home, &["update", "-t", "retail", "--dry-run"]);
    log.assert_eq(
        &format!("{label}: to release 2"),
        &to_versions(&previewed),
        &vec![landing.version(2)],
    );
    let (p, plan) = stored(log, &home, &previewed);

    log.step("release 3 lands inside the window");
    land(log, &mut home, &mut landing);
    let trees = Trees::record(log, &home);

    log.step("apply the stale plan");
    let before = requests(&home);
    let ran = run(log, &home, &["apply", &p]);
    stale_selection(log, &ran, &plan);
    trees.unchanged(log, &home);
    let cost = added(log, &home, &before);
    log.assert_eq(
        &format!("{label}: it revalidated ({})", landing.probe()),
        &(count(&cost, landing.probe()) > 0),
        &true,
    );

    log.step("re-preview inside the window");
    let before = requests(&home);
    let again = ok(log, &home, &["update", "-t", "retail", "--dry-run"]);
    log.assert_eq(
        &format!("{label}: to release 3"),
        &to_versions(&again),
        &vec![landing.version(3)],
    );
    let (p2, _) = stored(log, &home, &again);
    // ubs:ignore — comparing plan ids, not secrets
    log.assert_eq("a new plan", &(p2 != p), &true);
    if let Some(revision) = landing.revision(3) {
        // A re-tag keeps the version: the approved asset names the revision.
        let assets = words(
            &again.data()["approval"]["selection"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|item| item["asset"].clone())
                .collect(),
        );
        log.assert_eq(
            &format!("{label}: the approval names revision {revision}"),
            &assets,
            &vec![format!("bug-grabber/tags/v12.1.1@r{revision}")],
        );
    }
    let cost = added(log, &home, &before);
    log.assert_eq(
        &format!("{label}: the memo answered (no {})", landing.probe()),
        &count(&cost, landing.probe()),
        &0,
    );
    if source == Source::WowAce {
        log.note("WowAce assembles its archive from tree reads, logged above and not counted");
    } else {
        log.assert_eq(
            &format!("{label}: the stale apply cached the archive"),
            &count(&cost, "asset"),
            &0,
        );
    }

    log.step("apply the fresh plan");
    let before = requests(&home);
    let applied = ok(log, &home, &["apply", &p2]);
    committed_on_retail(log, &home, &applied);
    has_marker(log, &home, "_retail_", landing.marker(3));
    let addons: Vec<_> = record(&home, "retail").addons.into_values().collect();
    log.assert_eq("one addon", &addons.len(), &1);
    log.assert_eq(
        &format!("{label}: recorded at release 3"),
        &(addons[0].version.clone(), addons[0].revision),
        &(landing.version(3), landing.revision(3)),
    );
    log.assert_eq("history's plan", &newest_plan(log, &home), &json!(p2));
    let cost = added(log, &home, &before);
    log.assert_eq(
        &format!("{label}: one revalidation ({})", landing.probe()),
        &count(&cost, landing.probe()),
        &1,
    );
    if source == Source::GitHub {
        log.assert_eq(
            "the tag did not move: no release.json",
            &count(&cost, "release-json"),
            &0,
        );
    }
    if source != Source::WowAce {
        log.assert_eq(
            &format!("{label}: nothing downloaded"),
            &count(&cost, "asset"),
            &0,
        );
    }
    let diff = log.diff("addons", &home.addons("_retail_"));
    let prefix = landing.folder_prefix();
    let touched: Vec<&String> = diff
        .added
        .iter()
        .chain(&diff.removed)
        .chain(diff.changed.iter().map(|(path, _)| path))
        .collect();
    log.assert_eq(
        &format!("{label}: only the addon's folders ({prefix}…) changed"),
        &(!touched.is_empty() && touched.iter().all(|path| path.starts_with(prefix))),
        &true,
    );
}

// ---------------------------------------------------------------------------------------
// Per command, on GitHub.

#[test]
fn drift_install_lands_between_preview_and_apply() {
    let log = tlog!();
    let mut home = Home::new(&["wow"]);
    log.env(&home);

    log.step("preview");
    let mut landing = start(&log, &mut home, Source::GitHub);
    let previewed = ok(
        &log,
        &home,
        &["install", "gh:o/foo", "-t", "retail", "--dry-run"],
    );
    log.assert_eq("to v1", &to_versions(&previewed), &vec!["v1".to_owned()]);
    let token = previewed.data()["precondition"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let (p1, plan1) = stored(&log, &home, &previewed);

    log.step("release 2 lands");
    land(&log, &mut home, &mut landing);
    let trees = Trees::record(&log, &home);

    log.step("typed run of the old token");
    let typed = run(
        &log,
        &home,
        &["install", "gh:o/foo", "-t", "retail", "--if-match", &token],
    );
    typed_stale(&log, &typed);
    trees.unchanged(&log, &home);

    log.step("apply the stale plan");
    let ran = run(&log, &home, &["apply", &p1]);
    stale_selection(&log, &ran, &plan1);
    trees.unchanged(&log, &home);

    log.step("re-preview");
    let again = ok(
        &log,
        &home,
        &["install", "gh:o/foo", "-t", "retail", "--dry-run"],
    );
    log.assert_eq("to v2", &to_versions(&again), &vec!["v2".to_owned()]);
    let (p2, _) = stored(&log, &home, &again);

    log.step("apply the fresh plan");
    let before = requests(&home);
    let applied = ok(&log, &home, &["apply", &p2]);
    committed_on_retail(&log, &home, &applied);
    has_marker(&log, &home, "_retail_", landing.marker(2));
    log.assert_eq("history's plan", &newest_plan(&log, &home), &json!(p2));
    let cost = added(&log, &home, &before);
    log.assert_eq(
        "requests: one release-latest, no release-json, no download",
        &(
            count(&cost, "release-latest"),
            count(&cost, "release-json"),
            count(&cost, "asset"),
        ),
        &(1, 0, 0),
    );
}

/// Foo v1 installed on retail, v2 published and previewed by `update foo` (P, its
/// token), then v3 published inside the window.
fn update_home(log: &TestLog) -> (Home, Landing, String, String) {
    let mut home = Home::new(&["wow"]);
    log.env(&home);
    log.step("seed: v1 installed, v2 previewed, v3 published");
    let mut landing = start(log, &mut home, Source::GitHub);
    ok(log, &home, &["install", "gh:o/foo", "-t", "retail"]);
    land(log, &mut home, &mut landing);
    memo::age(&home, 24);
    let previewed = ok(log, &home, &["update", "foo", "-t", "retail", "--dry-run"]);
    log.assert_eq("to v2", &to_versions(&previewed), &vec!["v2".to_owned()]);
    let token = previewed.data()["precondition"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let (p, _) = stored(log, &home, &previewed);
    land(log, &mut home, &mut landing);
    (home, landing, p, token)
}

#[test]
fn drift_update_inside_the_window() {
    let log = tlog!();
    {
        let log = log.case("typed");
        let (home, landing, _, token) = update_home(&log);
        log.step("typed run of the token inside the window");
        let before = requests(&home);
        let typed = ok(
            &log,
            &home,
            &["update", "foo", "-t", "retail", "--if-match", &token],
        );
        log.assert_eq(
            "exactly the preview: v2",
            &to_versions(&typed),
            &vec!["v2".to_owned()],
        );
        has_marker(&log, &home, "_retail_", landing.marker(2));
        let cost = added(&log, &home, &before);
        log.assert_eq(
            "answered from the memo",
            &count(&cost, "release-latest"),
            &0,
        );
    }
    {
        let log = log.case("apply");
        let (home, landing, p, _) = update_home(&log);
        let plan = read_plan(&home.env, &p);
        let trees = Trees::record(&log, &home);
        log.step("apply the stale plan");
        let before = requests(&home);
        let ran = run(&log, &home, &["apply", &p]);
        stale_selection(&log, &ran, &plan);
        trees.unchanged(&log, &home);
        let cost = added(&log, &home, &before);
        log.assert_eq(
            "a probe and the moved tag's release.json",
            &(count(&cost, "release-latest"), count(&cost, "release-json")),
            &(1, 1),
        );

        log.step("re-preview inside the window");
        let before = requests(&home);
        let again = ok(&log, &home, &["update", "foo", "-t", "retail", "--dry-run"]);
        log.assert_eq("to v3", &to_versions(&again), &vec!["v3".to_owned()]);
        let cost = added(&log, &home, &before);
        log.assert_eq(
            "the memo the stale apply refreshed answered",
            &count(&cost, "release-latest"),
            &0,
        );
        let (p2, _) = stored(&log, &home, &again);

        log.step("apply the fresh plan");
        let applied = ok(&log, &home, &["apply", &p2]);
        committed_on_retail(&log, &home, &applied);
        has_marker(&log, &home, "_retail_", landing.marker(3));
        log.assert_eq("history's plan", &newest_plan(&log, &home), &json!(p2));
    }
}

/// `addons.toml` listing Foo for mainline clients, its `sync` previewed while v1 is the
/// newest (P, its token), then v2 published inside the window.
fn sync_home(log: &TestLog) -> (Home, Landing, String, String) {
    let mut home = Home::new(&["wow"]);
    log.env(&home);
    log.step("seed: addons.toml lists Foo, sync previewed at v1, v2 published");
    let mut landing = start(log, &mut home, Source::GitHub);
    home.write_desired("schema_version = \"1.0\"\n[family.mainline]\naddons = [\"gh:o/foo\"]\n");
    let previewed = ok(log, &home, &["sync", "--dry-run"]);
    log.assert_eq(
        "installs v1",
        &to_versions(&previewed),
        &vec!["v1".to_owned()],
    );
    let token = previewed.data()["precondition"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let (p, _) = stored(log, &home, &previewed);
    land(log, &mut home, &mut landing);
    (home, landing, p, token)
}

#[test]
fn drift_sync_inside_the_window() {
    let log = tlog!();
    {
        let log = log.case("typed");
        let (home, landing, _, token) = sync_home(&log);
        log.step("typed run of the token inside the window");
        let before = requests(&home);
        let typed = ok(&log, &home, &["sync", "--if-match", &token]);
        log.assert_eq(
            "exactly the preview: v1",
            &to_versions(&typed),
            &vec!["v1".to_owned()],
        );
        has_marker(&log, &home, "_retail_", landing.marker(1));
        let cost = added(&log, &home, &before);
        log.assert_eq(
            "answered from the memo",
            &count(&cost, "release-latest"),
            &0,
        );
    }
    {
        let log = log.case("apply");
        let (home, landing, p, _) = sync_home(&log);
        let plan = read_plan(&home.env, &p);
        let trees = Trees::record(&log, &home);
        log.step("apply the stale plan");
        let ran = run(&log, &home, &["apply", &p]);
        stale_selection(&log, &ran, &plan);
        trees.unchanged(&log, &home);
        log.assert_eq(
            "nothing installed",
            &home.addons("_retail_").join("Foo").exists(),
            &false,
        );

        log.step("re-preview inside the window");
        let before = requests(&home);
        let again = ok(&log, &home, &["sync", "--dry-run"]);
        log.assert_eq("installs v2", &to_versions(&again), &vec!["v2".to_owned()]);
        let cost = added(&log, &home, &before);
        log.assert_eq(
            "the memo the stale apply refreshed answered",
            &count(&cost, "release-latest"),
            &0,
        );
        let (p2, _) = stored(&log, &home, &again);

        log.step("apply the fresh plan");
        let applied = ok(&log, &home, &["apply", &p2]);
        committed_on_retail(&log, &home, &applied);
        has_marker(&log, &home, "_retail_", landing.marker(2));
    }
}

#[test]
fn drift_copy_revalidates_typed_and_applied() {
    let log = tlog!();
    let mut home = Home::new(&["wow", "wow_classic"]);
    log.env(&home);

    log.step("seed: v1 on retail, v2 published, copy previewed");
    let mut landing = start(&log, &mut home, Source::GitHub);
    ok(&log, &home, &["install", "gh:o/foo", "-t", "retail"]);
    land(&log, &mut home, &mut landing);
    let copy = ["copy", "--from", "retail", "--to", "classic"];
    let mut dry = copy.to_vec();
    dry.push("--dry-run");
    let previewed = ok(&log, &home, &dry);
    log.assert_eq(
        "v2 on classic",
        &to_versions(&previewed),
        &vec!["v2".to_owned()],
    );
    let token = previewed.data()["precondition"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let (p, plan) = stored(&log, &home, &previewed);

    log.step("release 3 lands");
    land(&log, &mut home, &mut landing);
    let trees = Trees::record(&log, &home);

    log.step("typed");
    let mut typed = copy.to_vec();
    typed.extend(["--if-match", &token]);
    let ran = run(&log, &home, &typed);
    typed_stale(&log, &ran);
    trees.unchanged(&log, &home);

    log.step("apply");
    let ran = run(&log, &home, &["apply", &p]);
    stale_selection(&log, &ran, &plan);
    trees.unchanged(&log, &home);

    log.step("re-preview, apply");
    let fix = words(&ran.envelope()["error"]["fix_argv"]);
    let fix: Vec<&str> = fix.iter().skip(1).map(String::as_str).collect();
    let again = ok(&log, &home, &fix);
    log.assert_eq(
        "v3 on classic",
        &to_versions(&again),
        &vec!["v3".to_owned()],
    );
    let (p2, _) = stored(&log, &home, &again);
    let applied = ok(&log, &home, &["apply", &p2]);
    let statuses: Vec<(Value, Value)> = applied.data()["outcomes"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|o| (o["target"].clone(), o["status"].clone()))
        .collect();
    log.assert_eq(
        "classic committed",
        &statuses,
        &vec![(json!("classic"), json!("committed"))],
    );
    has_marker(&log, &home, "_classic_", landing.marker(3));
    has_marker(&log, &home, "_retail_", landing.marker(1));
}

// ---------------------------------------------------------------------------------------
// A Tukui plan applied from the cache.

#[test]
fn drift_tukui_cache_hit_applies() {
    let log = tlog!();
    let mut home = Home::new(&["wow"]);
    log.env(&home);
    let mut seen: Vec<(String, String)> = Vec::new();
    let mut keep = |label: &str, ran: &Ran| {
        seen.push((format!("{label} stdout"), ran.stdout().to_owned()));
        seen.push((format!("{label} stderr"), ran.stderr().to_owned()));
    };

    log.step("preview downloads");
    let landing = start(&log, &mut home, Source::Tukui);
    let before = requests(&home);
    let previewed = ok(
        &log,
        &home,
        &["install", "tukui:elvui", "-t", "retail", "--dry-run"],
    );
    keep("preview", &previewed);
    let (p, _) = stored(&log, &home, &previewed);
    let cost = added(&log, &home, &before);
    log.assert_eq(
        "the dry run: the list, one resolve, one download",
        &(
            count(&cost, "tukui-list"),
            count(&cost, "tukui-addon"),
            count(&cost, "asset"),
        ),
        &(1, 1, 1),
    );

    log.step("apply from the cache");
    let before = requests(&home);
    let applied = ok(&log, &home, &["apply", &p, "-v"]);
    keep("apply", &applied);
    committed_on_retail(&log, &home, &applied);
    let item = &applied.data()["result"]["items"][0];
    log.json("the applied item", item);
    // `-v` reached the executed install (its `optional_dependencies` show), and a cache
    // hit resolved no URL, so `asset_url` is left out.
    log.assert_eq(
        "-v in effect, and a cache hit resolves no URL",
        &(
            item.get("optional_dependencies").is_some(),
            &item["asset_url"],
        ),
        &(true, &Value::Null),
    );
    let mut folders: Vec<String> = fs::read_dir(home.addons("_retail_"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    folders.sort();
    log.assert_eq(
        "ElvUI's folders",
        &folders,
        &vec![
            "ElvUI".to_owned(),
            "ElvUI_Libraries".to_owned(),
            "ElvUI_Options".to_owned(),
        ],
    );
    has_marker(&log, &home, "_retail_", landing.marker(1));
    let cost = added(&log, &home, &before);
    log.assert_eq(
        "the list alone",
        &cost,
        &BTreeMap::from([("tukui-list".to_owned(), 1)]),
    );

    log.step("no token anywhere");
    let plan =
        fs::read_to_string(crate::common::plans::plans_dir(&home.env).join(format!("{p}.json")))
            .unwrap();
    seen.push(("plan file".to_owned(), plan));
    let leaked: Vec<&String> = seen
        .iter()
        .filter(|(_, text)| text.contains("LAND1-"))
        .map(|(label, _)| label)
        .collect();
    log.note(format!(
        "checked {} texts for the download tokens LAND1-",
        seen.len()
    ));
    log.assert_eq("no download token", &leaked, &Vec::<&String>::new());
}
