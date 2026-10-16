//! Real-machine smoke tests for the M1 checkpoint (vision §5.12, §7): the real HOME and
//! XDG directories, never tempdirs. Ignored by default and additionally gated on
//! `OMADDONS_REAL_MACHINE=1`; the installs also need `OMADDONS_LIVE=1`:
//! `OMADDONS_REAL_MACHINE=1 OMADDONS_LIVE=1 cargo test --test real_machine -- --ignored --test-threads=1`
//!
//! Every transaction a test makes is undone before it ends, and every AddOns folder it
//! touched must hash as it did before. Target keys and markers written on the first
//! mutation stay, by design. A test skips (with a message) when its client is running.

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::PathBuf;

use common::trees::tree_hash;
use serde_json::Value;
use support::{
    addons_of, blizzard_hashes, clients, declares, enabled, entries, flavors_of, forever_guard,
    hashes, install_then_undo, net_log_lines, note, omaddons, recent_tags, skip_if_running,
    snapshot_all, target, target_where, tx_of, undo_all,
};

/// The live Classic clients: family, product, interface, version prefix.
const CLASSIC: &[(&str, &str, u32, &str)] = &[
    ("vanilla", "wow_classic_era", 11_509, "1.15."),
    ("tbc", "wow_anniversary", 20_506, "2.5."),
    ("mists", "wow_classic", 50_504, "5.5."),
];

#[cfg(test)]
mod support {
    use super::*;

    /// Whether the real-machine tests (and, with `live`, the network ones) may run.
    pub fn enabled(live: bool) -> bool {
        let on = |var: &str| std::env::var(var).as_deref() == Ok("1");
        on("OMADDONS_REAL_MACHINE") && (!live || on("OMADDONS_LIVE"))
    }

    /// `omaddons <args> --json` with the real environment and the real network:
    /// (exit code, envelope).
    pub fn omaddons(args: &[&str]) -> (i32, Value) {
        let mut cmd = assert_cmd::Command::cargo_bin("omaddons").unwrap();
        for var in [
            "OMADDONS_HTTP_FIXTURES",
            "OMADDONS_RECORD",
            "OMADDONS_PROC_ROOT",
            "OMADDONS_FAILPOINTS",
            "OMADDONS_OFFLINE",
        ] {
            cmd.env_remove(var);
        }
        let output = cmd.args(args).arg("--json").output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let envelope = serde_json::from_str(&stdout).unwrap_or(Value::Null);
        assert!(
            !envelope.is_null(),
            "omaddons {args:?} printed no envelope:\n{stdout}"
        );
        (output.status.code().unwrap_or(-1), envelope)
    }

    /// Evidence on stderr, with the home directory as `<home>`.
    pub fn note(label: &str, value: &Value) {
        let home = std::env::var("HOME").unwrap_or_default();
        let text = serde_json::to_string_pretty(value).unwrap_or_default();
        let text = if home.is_empty() {
            text
        } else {
            text.replace(&home, "<home>")
        };
        let _ = writeln!(std::io::stderr(), "== {label}\n{text}");
    }

    /// The status entry of the target whose family is `family`.
    pub fn target(status: &Value, family: &str) -> Value {
        target_where(status, "family", family)
    }

    /// The status entry of the first target whose `key` is `value`.
    pub fn target_where(status: &Value, key: &str, value: &str) -> Value {
        let found = status["data"]["targets"]
            .as_array()
            .and_then(|ts| ts.iter().find(|t| t[key] == value))
            .cloned();
        assert!(
            found.is_some(),
            "no target with {key} {value} in {status:#}"
        );
        found.unwrap()
    }

    /// Whether the newest stable release of `name` declares `flavor` in its
    /// release.json, as `info` reports it.
    pub fn declares(name: &str, flavor: &str) -> bool {
        flavors_of(name).iter().any(|f| f == flavor)
    }

    /// The flavors the newest stable release of `name` declares in its release.json.
    pub fn flavors_of(name: &str) -> Vec<String> {
        let (code, info) = omaddons(&["info", name]);
        assert_eq!(code, 0, "{info:#}");
        info["data"]["latest"]["flavors"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|f| f.as_str().map(str::to_owned))
            .collect()
    }

    /// Each dir's tree hash; empty for one that does not exist.
    pub fn hashes(dirs: &[PathBuf]) -> Vec<String> {
        dirs.iter()
            .map(|d| {
                if d.exists() {
                    tree_hash(d)
                } else {
                    String::new()
                }
            })
            .collect()
    }

    /// The entries of `dir`, sorted; empty when it does not exist.
    pub fn entries(dir: &std::path::Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .map(|l| {
                l.map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }

    /// The AddOns directory of target `id`, from `targets`.
    pub fn addons_of(id: &str) -> PathBuf {
        let (code, targets) = omaddons(&["targets"]);
        assert_eq!(code, 0, "{targets:#}");
        let dir = targets["data"]["targets"]
            .as_array()
            .and_then(|ts| ts.iter().find(|t| t["id"] == id))
            .and_then(|t| t["client_dir"].as_str())
            .map(|d| PathBuf::from(d).join("Interface/AddOns"));
        assert!(dir.is_some(), "no client dir for {id}: {targets:#}");
        dir.unwrap()
    }

    /// True (after saying why) when `target`'s client is running.
    pub fn skip_if_running(target: &Value) -> bool {
        let running = target["running"] == true;
        if running {
            let _ = writeln!(
                std::io::stderr(),
                "skipped: the {} client is running; quit it and run again",
                target["id"]
            );
        }
        running
    }

    /// Hashes of every `Blizzard_*` folder in `addons`.
    pub fn blizzard_hashes(addons: &std::path::Path) -> BTreeMap<String, String> {
        fs::read_dir(addons)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.to_ascii_lowercase().starts_with("blizzard_"))
            })
            .map(|p| {
                let name = p.file_name().unwrap().to_string_lossy().into_owned();
                (name, tree_hash(&p))
            })
            .collect()
    }

    /// Every registered client: (id, key, `Interface/AddOns`, `WTF`), from `targets`.
    pub fn clients() -> Vec<(String, String, PathBuf, PathBuf)> {
        let (code, targets) = omaddons(&["targets"]);
        assert_eq!(code, 0, "{targets:#}");
        targets["data"]["targets"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|t| {
                let dir = PathBuf::from(t["client_dir"].as_str()?);
                Some((
                    t["id"].as_str()?.to_owned(),
                    t["key"].as_str().unwrap_or_default().to_owned(),
                    dir.join("Interface/AddOns"),
                    dir.join("WTF"),
                ))
            })
            .collect()
    }

    /// Every client's `Interface/AddOns` and `WTF/` tree hash, by id (empty: absent).
    /// `WTF/` is hashed as a tree and never listed: its paths name accounts.
    pub fn snapshot_all() -> BTreeMap<String, (String, String)> {
        clients()
            .into_iter()
            .map(|(id, _, addons, wtf)| {
                let hashed = hashes(&[addons, wtf]);
                (id, (hashed[0].clone(), hashed[1].clone()))
            })
            .collect()
    }

    /// The Forever stress-test set's guard: `forever`'s record generation, its AddOns
    /// tree hash, and its managed addons and folders (5 and 12 while the set stays).
    pub fn forever_guard() -> Value {
        let Some((_, key, addons, _)) = clients().into_iter().find(|(id, ..)| id == "forever")
        else {
            return Value::Null;
        };
        let state = std::env::var_os("XDG_STATE_HOME").map_or_else(
            || PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".local/state"),
            PathBuf::from,
        );
        let record: Value = fs::read(state.join(format!("omaddons/targets/{key}.json")))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or(Value::Null);
        let (code, list) = omaddons(&["list", "-t", "forever"]);
        assert_eq!(code, 0, "{list:#}");
        let managed = list["data"]["targets"][0]["addons"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let folders: u64 = managed.iter().filter_map(|a| a["folders"].as_u64()).sum();
        serde_json::json!({
            "generation": record["generation"],
            "addons_hash": hashes(&[addons])[0],
            "addons": managed.len(),
            "folders": folders,
        })
    }

    /// The real state's `net.log` line count.
    pub fn net_log_lines() -> usize {
        let state = std::env::var_os("XDG_STATE_HOME").map_or_else(
            || PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".local/state"),
            PathBuf::from,
        );
        fs::read_to_string(state.join("omaddons/net.log"))
            .map(|t| t.lines().count())
            .unwrap_or(0)
    }

    /// Undoes `txs` by id, newest first, each of which must exit 0. On a failure it says
    /// which transactions are still applied and how to undo them, and fails.
    pub fn undo_all(txs: &[String]) {
        for (i, tx) in txs.iter().enumerate().rev() {
            let (code, undone) = omaddons(&["undo", tx]);
            note(&format!("undo {tx}"), &undone);
            let left: Vec<String> = txs[..=i]
                .iter()
                .rev()
                .map(|t| format!("omaddons undo {t}"))
                .collect();
            assert_eq!(
                code, 0,
                "undo {tx} failed; still applied, undo them in this order: {left:#?}"
            );
        }
    }

    /// The transaction an envelope committed: `data.tx`, else its only target's.
    pub fn tx_of(envelope: &Value) -> Option<String> {
        envelope["data"]["tx"]
            .as_str()
            .or_else(|| envelope["data"]["targets"][0]["tx"].as_str())
            .map(str::to_owned)
    }

    /// Recent stable tags of `owner/repo`, newest first, from its `releases.atom` (an
    /// isolated environment: nothing of it touches the real state).
    pub fn recent_tags(owner: &str, repo: &str) -> Vec<String> {
        use omaddons::net::{NetConfig, NetLog, stack};
        use omaddons::state::paths::Paths;
        let env = super::common::env::TestEnv::new();
        let paths = Paths::with_roots(env.home(), env.config(), env.state(), env.cache());
        let http = stack(&NetConfig::default(), NetLog::new(&paths).unwrap(), &paths);
        omaddons::sources::github::GithubSource::recent_tags(owner, repo, http.as_ref())
            .map(|t| t.tags.into_iter().map(|t| t.tag).collect::<Vec<String>>())
            .unwrap_or_default()
            .into_iter()
            .filter(|t| !t.contains('-') && !t.contains('/'))
            .collect()
    }

    /// Installs `refs` into `id` and undoes the transaction at once, so no failed check
    /// leaves the machine changed; then checks every item landed (those in `may_refuse`
    /// may instead fail with `no_compatible_release`, never another way).
    pub fn install_then_undo(id: &str, refs: &[&str], may_refuse: &[&str]) -> Value {
        let mut args = vec!["install"];
        args.extend_from_slice(refs);
        args.extend_from_slice(&["-t", id]);
        let (code, installed) = omaddons(&args);
        note(&format!("install {refs:?} -t {id}"), &installed);
        let tx = installed["data"]["tx"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let undo = (!tx.is_empty()).then(|| omaddons(&["undo", &tx]));
        if let Some((_, undone)) = &undo {
            note(&format!("undo {tx}"), undone);
        }
        assert_eq!(code, 0, "{installed:#}");
        assert!(!tx.is_empty(), "{installed:#}");
        assert!(undo.as_ref().is_some_and(|(c, _)| *c == 0), "{undo:#?}");
        // One item per ref; dependencies installed with them are items of their own.
        let items: Vec<Value> = installed["data"]["items"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|i| refs.iter().any(|r| i["request"] == *r))
            .cloned()
            .collect();
        assert_eq!(items.len(), refs.len(), "{installed:#}");
        for item in &items {
            let refused = item["status"] == "failed"
                && item["error"]["code"] == "no_compatible_release"
                && may_refuse.iter().any(|r| item["request"] == *r);
            assert!(item["status"] == "installed" || refused, "{item:#}");
        }
        installed
    }
}

#[test]
#[ignore = "real machine: OMADDONS_REAL_MACHINE=1"]
fn status_reports_every_client() {
    if !enabled(false) {
        return;
    }
    let (code, status) = omaddons(&["status"]);
    note("status", &status);
    assert_eq!(code, 0, "{status:#}");

    // The WoW: Forever stress-test client (installed 2026-10-15).
    let forever = target_where(&status, "product", "wowf");
    assert_eq!(
        (&forever["family"], &forever["interface"], &forever["kind"]),
        (
            &Value::from("forever"),
            &Value::from(16_001),
            &Value::from("live")
        ),
        "{forever:#}"
    );
    assert!(
        forever["version"]
            .as_str()
            .unwrap_or("")
            .starts_with("1.60."),
        "{forever:#}"
    );
    let id = forever["id"].as_str().unwrap_or("forever").to_owned();
    let (code, list) = omaddons(&["list", "-t", &id]);
    assert_eq!(code, 0, "{list:#}");
    assert_eq!(
        list["data"]["targets"][0]["addons_dir_missing"],
        !addons_of(&id).is_dir(),
        "list reports the AddOns folder as it is on disk: {list:#}"
    );

    let retail = target(&status, "mainline");
    assert_eq!(
        (&retail["id"], &retail["product"], &retail["kind"]),
        (
            &Value::from("retail"),
            &Value::from("wow"),
            &Value::from("live")
        )
    );
    assert!(
        retail["version"]
            .as_str()
            .unwrap_or("")
            .starts_with("12.1."),
        "{retail:#}"
    );
    let wotlk = target(&status, "wrath-legacy");
    assert_eq!(
        (
            &wotlk["interface"],
            &wotlk["label"],
            &wotlk["folders"]["protected"]
        ),
        (
            &Value::from(30_300),
            &Value::from("localhost"),
            &Value::from(23)
        )
    );

    // The live Classic clients (each booted once on 2026-10-15).
    for (family, product, interface, prefix) in CLASSIC {
        let t = target(&status, family);
        assert_eq!(
            (&t["product"], &t["interface"], &t["kind"]),
            (
                &Value::from(*product),
                &Value::from(*interface),
                &Value::from("live")
            ),
            "{t:#}"
        );
        assert!(
            t["version"].as_str().unwrap_or("").starts_with(prefix),
            "{t:#}"
        );
        assert_eq!(
            (&t["attention"], &t["folders"]["invalid"]),
            (&serde_json::json!([]), &Value::from(0)),
            "{t:#}"
        );
        if t["running"] == true {
            note(
                &format!("{family} is running; not asserting it is idle"),
                &t,
            );
        }
    }

    // Classic Era, Hardcore and Season of Discovery share the Classic Era client.
    let era = target(&status, "vanilla");
    let modes: Vec<&str> = era["modes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| m["name"].as_str())
        .collect();
    assert_eq!(
        modes,
        ["Classic Era", "Hardcore", "Season of Discovery"],
        "{era:#}"
    );
    for (selector, family) in [
        ("sod", "vanilla"),
        ("hardcore", "vanilla"),
        ("mop", "mists"),
    ] {
        let (code, one) = omaddons(&["status", "-t", selector]);
        assert_eq!(code, 0, "{selector}: {one:#}");
        assert_eq!(
            one["data"]["targets"][0]["id"],
            target(&status, family)["id"],
            "-t {selector} selects the {family} client"
        );
    }
}

#[test]
#[ignore = "real machine and live network: OMADDONS_REAL_MACHINE=1 OMADDONS_LIVE=1"]
fn keyless_installs_retail() {
    if !enabled(true) {
        return;
    }
    let (_, status) = omaddons(&["status"]);
    let retail = target(&status, "mainline");
    if skip_if_running(&retail) {
        return;
    }
    let addons = addons_of("retail");
    let before = tree_hash(&addons);
    // WeakAuras 5.22 publishes no mainline build (Midnight is unsupported upstream), so
    // the only right outcome for it on retail is a refusal; it must never install wrong.
    install_then_undo(
        "retail",
        &["weakauras", "dbm", "bigwigs", "plater", "bartender4"],
        &["weakauras"],
    );
    assert_eq!(tree_hash(&addons), before, "retail AddOns as before");
    let (_, after) = omaddons(&["status"]);
    let retail = target(&after, "mainline");
    assert!(
        retail["key"].is_string(),
        "registered on first mutation: {retail:#}"
    );
}

/// Every live Classic client (era, anniversary, classic) takes the keyless set: each
/// addon installs as its publisher declares the client's flavor, DBM and BigWigs skip
/// their retail-only modules, nothing writes `WTF/`, and undoing one client never
/// touches another. Nothing is asserted while the machine is changed: every install is
/// undone first, newest first, and the recorded results are checked afterwards, so a
/// failed check never leaves addons behind.
#[test]
#[ignore = "real machine and live network: OMADDONS_REAL_MACHINE=1 OMADDONS_LIVE=1"]
fn keyless_installs_classic() {
    if !enabled(true) {
        return;
    }
    let started = jiff::Timestamp::now();
    let (_, status) = omaddons(&["status"]);
    let clients: Vec<Value> = CLASSIC.iter().map(|c| target(&status, c.0)).collect();
    if clients.iter().any(skip_if_running) {
        return;
    }
    let ids: Vec<String> = clients
        .iter()
        .map(|t| t["id"].as_str().unwrap_or_default().to_owned())
        .collect();
    let addons: Vec<PathBuf> = ids.iter().map(|id| addons_of(id)).collect();
    let wtf: Vec<PathBuf> = addons
        .iter()
        .map(|a| a.parent().and_then(|i| i.parent()).unwrap().join("WTF"))
        .collect();
    let (before, wtf_before) = (hashes(&addons), hashes(&wtf));
    let refs = [
        "weakauras",
        "dbm",
        "bigwigs",
        "questie",
        "bartender4",
        "omnicc",
    ];
    let flavors: Vec<(&str, Vec<String>)> = refs.iter().map(|r| (*r, flavors_of(r))).collect();

    // Install into each client, recording without asserting.
    let mut installs: Vec<(i32, Value, Value, Vec<String>)> = Vec::new();
    for id in &ids {
        let mut args = vec!["install"];
        args.extend(refs);
        args.extend(["-t", id.as_str()]);
        let (code, installed) = omaddons(&args);
        note(&format!("install -t {id}"), &installed);
        let (_, after) = omaddons(&["status", "-t", id]);
        installs.push((code, installed, after, hashes(&addons)));
    }
    // Undo newest first, hashing every client after each undo.
    let mut undos: Vec<Option<(i32, Vec<String>)>> = Vec::new();
    for (_, installed, _, _) in installs.iter().rev() {
        let Some(tx) = installed["data"]["tx"].as_str() else {
            undos.push(None);
            continue;
        };
        let (code, undone) = omaddons(&["undo", tx]);
        note(&format!("undo {tx}"), &undone);
        undos.push(Some((code, hashes(&addons))));
    }
    undos.reverse();

    // Now the checks.
    for (i, (code, installed, after, _)) in installs.iter().enumerate() {
        let (id, flavor) = (&ids[i], ["classic", "bcc", "mists"][i]);
        for (r, declared) in &flavors {
            assert!(
                declared.iter().any(|f| f == flavor),
                "{r} no longer declares the {flavor} flavor, so it cannot install on {id}"
            );
        }
        assert_eq!(*code, 0, "{id}: {installed:#}");
        let items = installed["data"]["items"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let mut managed = 0;
        for item in &items {
            let request = item["request"].as_str().unwrap_or_default();
            assert_eq!(item["status"], "installed", "{request} on {id}: {item:#}");
            let folders = &item["folders"];
            managed += folders["added"].as_array().map_or(0, Vec::len);
            let core = match request {
                "dbm" => "DBM-Core",
                "bigwigs" => "BigWigs_Core",
                _ => continue,
            };
            let skipped = folders["skipped"].as_array().cloned().unwrap_or_default();
            assert!(
                folders["added"]
                    .as_array()
                    .is_some_and(|a| a.iter().any(|f| f == core))
                    && !skipped.is_empty()
                    && skipped.iter().all(|s| s["reason"] == "game_type_excluded"),
                "{request} on {id}: shared folders added, retail-only modules skipped: {folders:#}"
            );
        }
        let counts = &after["data"]["targets"][0]["folders"];
        assert_eq!(
            (&counts["managed"], &counts["invalid"]),
            (&Value::from(managed), &Value::from(0)),
            "{id} after installing: {after:#}"
        );
        // Undoing this client restored it and left the clients installed before it.
        let (undone, now) = undos[i].clone().expect("every install made a transaction");
        assert_eq!(undone, 0, "undo on {id}");
        for k in 0..ids.len() {
            let expected = if k < i { &installs[k].3[k] } else { &before[k] };
            assert_eq!(
                &now[k], expected,
                "after undoing {id}, {} is as expected",
                ids[k]
            );
        }
    }
    assert_eq!(hashes(&addons), before, "every AddOns as before");
    assert_eq!(hashes(&wtf), wtf_before, "nothing wrote WTF/");
    let paths = omaddons::state::paths::Paths::from_env().unwrap();
    let requests = omaddons::net::NetLog::new(&paths)
        .unwrap()
        .read(Some(started))
        .unwrap();
    assert!(
        !requests.iter().any(|e| e.host == "api.github.com"),
        "the stable channel needs no REST"
    );
}

/// The real WoW: Forever stress-test client (`wowf`, `_forever_`). Each keyless publisher
/// installs exactly when its release.json declares `forever`; DBM and BigWigs skip their
/// retail-only modules (game type `standard`, Forever's is `camelot`); a client never
/// launched gets its AddOns folder inside the transaction. Everything is undone: AddOns
/// ends as found, except that a folder this test created stays, empty.
#[test]
#[ignore = "real machine and live network: OMADDONS_REAL_MACHINE=1 OMADDONS_LIVE=1"]
fn keyless_installs_forever() {
    if !enabled(true) {
        return;
    }
    let (_, status) = omaddons(&["status"]);
    let forever = target_where(&status, "product", "wowf");
    if skip_if_running(&forever) {
        return;
    }
    // Installing what is already installed is a no-op with nothing to undo, so the test
    // needs a client whose addons it puts there itself (the stress-test set stays
    // installed for the owner's sessions; omad-classic-forever-gaps-xud.6).
    let managed = forever["addons"]["total"].as_u64().unwrap_or(0);
    if managed > 0 {
        let _ = writeln!(
            std::io::stderr(),
            "skipped: the forever client already manages {managed} addons; remove or undo them and run again"
        );
        return;
    }
    let id = forever["id"].as_str().unwrap_or("forever").to_owned();
    let addons = addons_of(&id);
    let had_addons = addons.is_dir();
    let before = had_addons.then(|| tree_hash(&addons));
    note(
        "forever before",
        &serde_json::json!({ "addons_dir_exists": had_addons, "version": forever["version"] }),
    );

    let refs = ["plater", "bartender4", "dbm", "bigwigs", "questie"];
    let declared: Vec<&str> = refs
        .iter()
        .copied()
        .filter(|r| declares(r, "forever"))
        .collect();
    let undeclared: Vec<&str> = refs
        .iter()
        .copied()
        .filter(|r| !declared.contains(r))
        .collect();
    note("declares forever", &serde_json::json!(declared));
    let installed = install_then_undo(&id, &refs, &undeclared);
    assert_eq!(
        installed["data"]["creates_addons_dir"], !had_addons,
        "the AddOns folder is created exactly when it was missing: {installed:#}"
    );
    for item in installed["data"]["items"].as_array().into_iter().flatten() {
        let request = item["request"].as_str().unwrap_or_default();
        let ok = item["status"] == "installed";
        assert_eq!(
            ok,
            declared.contains(&request),
            "{request}: installs exactly when its release.json declares forever: {item:#}"
        );
        if ok && ["dbm", "bigwigs"].contains(&request) {
            let skipped = item["folders"]["skipped"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            assert!(
                !skipped.is_empty() && skipped.iter().all(|s| s["reason"] == "game_type_excluded"),
                "{request}: retail-only modules skipped by game type: {item:#}"
            );
        }
    }

    // WeakAuras installs only once its release.json declares forever.
    let weakauras = declares("weakauras", "forever");
    let (code, wa) = omaddons(&["install", "weakauras", "-t", &id]);
    note(&format!("weakauras (declares forever: {weakauras})"), &wa);
    if let Some(tx) = wa["data"]["tx"].as_str() {
        let (undone, _) = omaddons(&["undo", tx]);
        assert_eq!(undone, 0);
    }
    if weakauras {
        assert_eq!(code, 0, "{wa:#}");
        assert_eq!(wa["data"]["items"][0]["status"], "installed", "{wa:#}");
    } else {
        assert_eq!(wa["error"]["code"], "no_compatible_release", "{wa:#}");
    }

    match before {
        Some(hash) => assert_eq!(tree_hash(&addons), hash, "forever AddOns as before"),
        None => assert_eq!(
            entries(&addons),
            Vec::<String>::new(),
            "the created AddOns folder stays, empty"
        ),
    }
}

#[test]
#[ignore = "real machine and live network: OMADDONS_REAL_MACHINE=1 OMADDONS_LIVE=1"]
fn legacy_installs_335_from_git_and_from_a_release() {
    if !enabled(true) {
        return;
    }
    let (_, status) = omaddons(&["status"]);
    let wotlk = target(&status, "wrath-legacy");
    if skip_if_running(&wotlk) {
        return;
    }
    let id = wotlk["id"].as_str().unwrap_or("wotlk").to_owned();
    let addons = addons_of(&id);
    let before = tree_hash(&addons);
    let blizzard = blizzard_hashes(&addons);
    assert_eq!(blizzard.len(), 23);
    // The catalog's GroupBulletinBoard is a git ref; the same addon's GitHub release is
    // a zip-only release listed through the REST fallback.
    let from_git = install_then_undo(&id, &["gbb"], &[]);
    assert!(
        from_git["data"]["items"][0]["ref"]
            .as_str()
            .is_some_and(|r| r.starts_with("git:"))
    );
    install_then_undo(&id, &["gh:fondlez/GroupBulletinBoard"], &[]);
    assert_eq!(blizzard_hashes(&addons), blizzard, "Blizzard_* untouched");
    assert_eq!(tree_hash(&addons), before, "wotlk AddOns as before");
}

// M2 (H5). Rules: read-only on every client (forever included); mutations only with an
// explicit `-t` on retail or a Classic client, never forever; no bare `update`, no
// non-dry-run `sync`, no `adopt --apply`, no `pin`/`unpin` without `-t`; every
// transaction undone by id, newest first, before anything is asserted; every client's
// AddOns and WTF/ hash as before, and the Forever stress-test set stays untouched.

/// `outdated`, `adopt --no-verify` (a report), `status` and `export` on every client.
#[test]
#[ignore = "real machine and live network: OMADDONS_REAL_MACHINE=1 OMADDONS_LIVE=1"]
fn m2_read_only_sweep_on_every_client() {
    if !enabled(true) {
        return;
    }
    let (guard, snapshot) = (forever_guard(), snapshot_all());
    note("forever guard", &guard);
    for (id, ..) in clients() {
        let (code, outdated) = omaddons(&["outdated", "-t", &id]);
        assert_eq!(code, 0, "outdated -t {id}: {outdated:#}");
        if id == "forever" {
            note(
                "forever: outdated items",
                &outdated["data"]["targets"][0]["items"],
            );
        }
        let (code, adopt) = omaddons(&["adopt", "--no-verify", "-t", &id]);
        assert_eq!(code, 0, "adopt -t {id}: {adopt:#}");
        let groups: Vec<Value> = adopt["data"]["targets"][0]["groups"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|g| serde_json::json!({"name": g["name"], "tier": g["tier"]}))
            .collect();
        note(&format!("{id}: adopt groups"), &Value::from(groups));
    }
    let (code, status) = omaddons(&["status"]);
    assert_eq!(code, 0, "{status:#}");
    let drift: Vec<Value> = status["data"]["targets"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|t| serde_json::json!({"id": t["id"], "drift": t["drift"]}))
        .collect();
    note("drift per target", &Value::from(drift));
    let (code, export) = omaddons(&["export"]);
    assert_eq!(code, 0, "{export:#}");
    assert_eq!(snapshot_all(), snapshot, "every AddOns and WTF/ as before");
    assert_eq!(forever_guard(), guard, "the Forever set untouched");
}

/// On era: Dominos (not in the Forever set) installed pinned at an older tag, unpinned,
/// updated, and the three undone.
#[test]
#[ignore = "real machine and live network: OMADDONS_REAL_MACHINE=1 OMADDONS_LIVE=1"]
fn m2_pin_unpin_update_on_era() {
    if !enabled(true) {
        return;
    }
    let (_, status) = omaddons(&["status"]);
    if skip_if_running(&target_where(&status, "id", "era")) {
        return;
    }
    let (guard, snapshot) = (forever_guard(), snapshot_all());
    let tags = recent_tags("tullamods", "Dominos");
    note("Dominos tags", &serde_json::json!(tags));
    let mut txs: Vec<String> = Vec::new();
    let mut steps: Vec<Value> = Vec::new();
    // An older tag that loads on era, pinned.
    let mut pinned: Option<String> = None;
    for tag in tags.iter().skip(1).take(3) {
        let (code, installed) = omaddons(&["install", "dominos", "--version", tag, "-t", "era"]);
        note(
            &format!("install dominos --version {tag} -t era"),
            &installed,
        );
        txs.extend(tx_of(&installed));
        if code == 0 && installed["data"]["items"][0]["status"] == "installed" {
            steps.push(installed["data"]["items"][0].clone());
            pinned = Some(tag.clone());
            break;
        }
    }
    if pinned.is_some() {
        let (_, unpinned) = omaddons(&["unpin", "Dominos", "-t", "era"]);
        note("unpin Dominos -t era", &unpinned);
        txs.extend(tx_of(&unpinned));
        steps.push(unpinned["data"]["targets"][0]["items"][0].clone());
        let (_, updated) = omaddons(&["update", "Dominos", "-t", "era"]);
        note("update Dominos -t era", &updated);
        txs.extend(tx_of(&updated));
        steps.push(updated["data"]["targets"][0]["items"][0].clone());
    }
    undo_all(&txs);
    assert_eq!(snapshot_all(), snapshot, "every AddOns and WTF/ as before");
    assert_eq!(forever_guard(), guard, "the Forever set untouched");
    assert!(
        pinned.is_some(),
        "no older Dominos tag installs on era: {tags:?}"
    );
    let statuses: Vec<&Value> = steps.iter().map(|s| &s["status"]).collect();
    assert_eq!(statuses, ["installed", "unpinned", "updated"], "{steps:#?}");
    assert_eq!(txs.len(), 3, "three transactions, three undos");
}

/// On retail: LittleWigs and its dependency BigWigs in one transaction, one undo.
#[test]
#[ignore = "real machine and live network: OMADDONS_REAL_MACHINE=1 OMADDONS_LIVE=1"]
fn m2_dependency_install_on_retail() {
    if !enabled(true) {
        return;
    }
    let (_, status) = omaddons(&["status"]);
    if skip_if_running(&target_where(&status, "id", "retail")) {
        return;
    }
    let (guard, snapshot) = (forever_guard(), snapshot_all());
    let by_hand = addons_of("retail").join("BigWigs").exists();
    let (code, installed) = omaddons(&["install", "littlewigs", "-t", "retail"]);
    note("install littlewigs -t retail", &installed);
    let txs: Vec<String> = tx_of(&installed).into_iter().collect();
    undo_all(&txs);
    assert_eq!(snapshot_all(), snapshot, "every AddOns and WTF/ as before");
    assert_eq!(forever_guard(), guard, "the Forever set untouched");
    assert_eq!(code, 0, "{installed:#}");
    assert_eq!(txs.len(), 1, "one transaction");
    let items = installed["data"]["items"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if by_hand {
        // Satisfied by the folder on disk: nothing installed for it, a hint to adopt.
        assert_eq!(items.len(), 1, "{installed:#}");
        note("BigWigs present by hand", &installed["commands"]);
    } else {
        let shown: Vec<(&Value, &Value, &Value, &Value)> = items
            .iter()
            .map(|i| (&i["request"], &i["status"], &i["reason"], &i["required_by"]))
            .collect();
        assert_eq!(
            shown,
            [
                (
                    &Value::from("littlewigs"),
                    &Value::from("installed"),
                    &Value::from("explicit"),
                    &serde_json::json!([])
                ),
                (
                    &Value::from("BigWigs"),
                    &Value::from("installed"),
                    &Value::from("dependency"),
                    &serde_json::json!(["LittleWigs"])
                )
            ],
            "{installed:#}"
        );
    }
}

/// `sync --dry-run` of a fresh export plans no change on any client and asks nothing.
#[test]
#[ignore = "real machine and live network: OMADDONS_REAL_MACHINE=1 OMADDONS_LIVE=1"]
fn m2_export_then_sync_dry_run_changes_nothing() {
    if !enabled(true) {
        return;
    }
    let (guard, snapshot) = (forever_guard(), snapshot_all());
    let (code, export) = omaddons(&["export"]);
    assert_eq!(code, 0, "{export:#}");
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("addons.toml");
    fs::write(
        &file,
        export["data"]["content"].as_str().unwrap_or_default(),
    )
    .unwrap();
    let requests = net_log_lines();
    let path = file.display().to_string();
    let (code, synced) = omaddons(&["sync", &path, "--dry-run"]);
    note("sync <export> --dry-run", &synced["data"]["summary"]);
    assert_eq!(code, 0, "{synced:#}");
    assert_eq!(synced["data"]["summary"]["changed"], 0, "{synced:#}");
    assert_eq!(net_log_lines(), requests, "sync asked nothing");
    assert_eq!(snapshot_all(), snapshot, "every AddOns and WTF/ as before");
    assert_eq!(forever_guard(), guard, "the Forever set untouched");
}
