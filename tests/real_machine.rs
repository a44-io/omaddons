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
    addons_of, blizzard_hashes, enabled, install_then_undo, note, omaddons, skip_if_running, target,
};

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
        let found = status["data"]["targets"]
            .as_array()
            .and_then(|ts| ts.iter().find(|t| t["family"] == family))
            .cloned();
        assert!(found.is_some(), "no {family} target in {status:#}");
        found.unwrap()
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
        let items = installed["data"]["items"]
            .as_array()
            .cloned()
            .unwrap_or_default();
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
fn status_reports_retail_and_335() {
    if !enabled(false) {
        return;
    }
    let (code, status) = omaddons(&["status"]);
    note("status", &status);
    assert_eq!(code, 0, "{status:#}");
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
