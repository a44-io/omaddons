//! `omaddons info` and `omaddons net-log` end to end: catalog lookups with replayed
//! GitHub releases, installed records, offline and failing fetches; the request log
//! with its summary, `--since`, a corrupt line, the rotated file and a planted secret.

mod common;

use std::fs;

use common::archives::zip_text;
use common::home::Home;
use common::http::{Asset, fixture, gh_release};
use common::trees::tree_hash;
use serde_json::Value;
use support::weakauras;

#[cfg(test)]
mod support {
    use super::*;

    pub const WA: &str = "https://github.com/WeakAuras/WeakAuras2";

    /// WeakAuras 5.19.2 with one asset for mainline and mists.
    pub fn weakauras(home: &Home) {
        let toc = "## Interface: 120105, 50504\n## Version: 5.19.2\n";
        let assets: [Asset<'_>; 1] = [(
            "WeakAuras-5.19.2.zip",
            &[("mainline", 120_105), ("mists", 50_504)],
            vec![("WeakAuras/WeakAuras.toc", toc)],
        )];
        gh_release(&home.http, WA, "5.19.2", &assets);
    }
}

fn net_log(home: &Home) -> String {
    fs::read_to_string(home.state().join("net.log")).unwrap_or_default()
}

#[test]
fn elvui_by_its_catalog_names_is_tukuis() {
    let home = Home::new(&["wow", "wow_classic_era"]);
    common::tukui::tukui_list(&home.http, &[common::tukui::elvui()]);
    for name in ["elvui", "elv"] {
        let r = home.run(&["info", name]);
        let envelope = r.expect(0);
        let data = &envelope["data"];
        assert_eq!(
            (&data["ref"], &data["catalog"]["name"]),
            (&Value::from("tukui:elvui"), &Value::from("ElvUI")),
            "{}",
            r.context()
        );
        assert_eq!(
            data["catalog"]["folders"],
            serde_json::json!(["ElvUI", "ElvUI_Libraries", "ElvUI_Options"])
        );
        assert_eq!(data["latest"]["version"], "16.00", "{}", r.context());
        assert_eq!(envelope["partial"], false);
    }
}

#[test]
fn info_reports_the_catalog_latest_and_installed() {
    let home = Home::new(&["wow", "wow_classic"]);
    weakauras(&home);
    let r = home.run(&["info", "weakauras"]);
    let envelope = r.expect(0);
    let data = &envelope["data"];
    assert_eq!(
        (&data["ref"], &data["name"], &data["catalog"]["name"]),
        (
            &Value::from("gh:WeakAuras/WeakAuras2"),
            &Value::from("WeakAuras"),
            &Value::from("WeakAuras")
        ),
        "{}",
        r.context()
    );
    let latest = &data["latest"];
    assert_eq!(latest["version"], "5.19.2");
    assert_eq!(latest["flavors"], serde_json::json!(["mainline", "mists"]));
    assert_eq!(latest["targets"], serde_json::json!(["classic", "retail"]));
    assert_eq!(data["installed"], serde_json::json!([]));
    assert_eq!(data["offline"], false);

    let r = home.run(&["install", "weakauras", "-t", "retail"]);
    r.expect(0);
    let envelope = home.run(&["info", "weakauras"]).expect(0);
    let installed = &envelope["data"]["installed"];
    assert_eq!(installed.as_array().unwrap().len(), 1, "{envelope}");
    assert_eq!(
        (
            &installed[0]["target"]["id"],
            &installed[0]["version"],
            &installed[0]["state"]
        ),
        (
            &Value::from("retail"),
            &Value::from("5.19.2"),
            &Value::from("ok")
        )
    );

    // Offline: the latest release as remembered, said to be unchecked, and no request.
    let log = net_log(&home);
    let r = home.run(&["info", "weakauras", "--offline"]);
    let envelope = r.expect(0);
    assert_eq!(
        (
            &envelope["data"]["latest"]["version"],
            &envelope["data"]["offline"]
        ),
        (&Value::from("5.19.2"), &Value::Bool(true)),
        "{}",
        r.context()
    );
    let warning = &envelope["warnings"][0];
    assert_eq!(
        (&warning["code"], &warning["fix"]),
        (
            &Value::from("stale_check"),
            &Value::from("run without --offline and with OMADDONS_OFFLINE unset")
        ),
        "{}",
        r.context()
    );
    assert_eq!(envelope["data"]["installed"].as_array().unwrap().len(), 1);
    assert_eq!(net_log(&home), log, "net.log unchanged");

    // Nothing remembered: the local part only.
    let fresh = Home::new(&["wow"]);
    weakauras(&fresh);
    let r = fresh.run(&["info", "weakauras", "--offline"]);
    let envelope = r.expect(0);
    assert_eq!(
        (&envelope["data"]["latest"], &envelope["warnings"]),
        (&Value::Null, &serde_json::json!([])),
        "{}",
        r.context()
    );
    assert_eq!(net_log(&fresh), "", "no request");
}

#[test]
fn info_errors_and_a_failing_fetch() {
    let home = Home::new(&["wow"]);
    let r = home.run(&["info", "nosuchaddon"]);
    let envelope = r.expect(3);
    assert_eq!(envelope["error"]["code"], "not_found", "{}", r.context());
    assert_eq!(
        envelope["error"]["fix_argv"],
        serde_json::json!(["omaddons", "list"])
    );

    let r = home.run(&["info", "wago:aNDmy96o"]);
    let envelope = r.expect(1);
    assert_eq!(
        envelope["error"]["code"],
        "source_disabled",
        "{}",
        r.context()
    );
    assert_eq!(
        envelope["error"]["fix_argv"],
        serde_json::json!(["omaddons", "help", "install"])
    );

    // Installed locally, but the source answers 503 every time: partial.
    let dl = home.env.root().join("dl");
    fs::create_dir_all(&dl).unwrap();
    let zip = dl.join("Foo.zip");
    fs::write(&zip, zip_text(&[("Foo/Foo.toc", "## Interface: 120105\n")])).unwrap();
    let path = zip.display().to_string();
    home.run(&["install", "gh:o/foo", "--file", &path, "-t", "retail"])
        .expect(0);
    fixture(
        &home.http,
        "foo-latest-503",
        "https://github.com/o/foo/releases/latest",
        503,
        &[("retry-after", "0")],
        None,
    );
    let r = home.run(&["info", "gh:o/foo"]);
    let envelope = r.expect(0);
    assert_eq!(
        (&envelope["partial"], &envelope["data"]["latest"]),
        (&Value::Bool(true), &Value::Null),
        "{}",
        r.context()
    );
    assert_eq!(envelope["data"]["installed"].as_array().unwrap().len(), 1);
    let codes: Vec<&str> = envelope["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|w| w["code"].as_str())
        .collect();
    assert!(codes.contains(&"http_status"), "{}", r.context());
}

#[test]
fn net_log_lists_summarizes_filters_and_keeps_secrets_out() {
    let home = Home::new(&["wow"]);
    weakauras(&home);
    home.run(&["install", "weakauras", "-t", "retail"])
        .expect(0);
    // A URL with fake credentials planted in its query: requested, logged without them.
    // ubs:ignore — fake planted values; the test asserts they never reach net.log
    let planted = "https://github.com/o/planted/releases/download/v1/P.zip?access_token=PLANTED1&api-key=PLANTED2&token=PLANTED3";
    let zip = zip_text(&[("Planted/Planted.toc", "## Interface: 120105\n")]);
    common::http::fixture_bytes(&home.http, "planted", planted, 200, &[], Some(&zip));
    let installed = home.run(&["install", &format!("url:{planted}"), "-t", "retail"]);
    assert_eq!(installed.code, 0, "{}", installed.context());

    let r = home.run(&["net-log"]);
    let envelope = r.expect(0);
    let entries = envelope["data"]["entries"].as_array().unwrap();
    assert!(
        entries
            .iter()
            .any(|e| e["path"].eq("/o/planted/releases/download/v1/P.zip")),
        "the planted request was made and logged by path only\n{}",
        r.context()
    );
    assert!(
        entries
            .iter()
            .any(|e| e["host"] == "github.com" && e["via"] == "http"),
        "{}",
        r.context()
    );
    let github = entries.iter().filter(|e| e["host"] == "github.com").count();
    assert_eq!(envelope["data"]["summary"]["by_host"]["github.com"], github);
    assert_eq!(envelope["data"]["summary"]["by_via"]["http"], entries.len());
    let raw = format!("{}\n{}", r.stdout, net_log(&home));
    for planted in [
        "PLANTED1",
        "PLANTED2",
        "PLANTED3",
        "access_token",
        "api-key",
        "Authorization",
    ] {
        assert!(!raw.contains(planted), "{planted} leaked:\n{raw}");
    }

    // --since filters; nonsense is usage.
    let all = entries.len();
    let recent = home.run(&["net-log", "--since", "1h"]).expect(0);
    assert_eq!(recent["data"]["entries"].as_array().unwrap().len(), all);
    let future = home
        .run(&["net-log", "--since", "2030-01-01T00:00:00Z"])
        .expect(0);
    assert_eq!(future["data"]["entries"], serde_json::json!([]));
    let r = home.run(&["net-log", "--since", "nonsense"]);
    let envelope = r.expect(2);
    assert_eq!(envelope["error"]["code"], "usage");
    assert_eq!(
        envelope["error"]["fix_argv"],
        serde_json::json!(["omaddons", "net-log", "--since", "1d"])
    );

    // A corrupt line is skipped and counted; the rotated file is read first.
    let log = home.state().join("net.log");
    let first = net_log(&home).lines().next().unwrap().to_owned();
    let mut older: Value = serde_json::from_str(&first).unwrap();
    older["time"] = Value::from("2026-01-01T00:00:00Z");
    older["path"] = Value::from("/rotated");
    fs::write(home.state().join("net.log.1"), format!("{older}\n")).unwrap();
    fs::write(&log, format!("{}not json\n", net_log(&home))).unwrap();
    let before = tree_hash(&home.state());
    let envelope = home.run(&["net-log"]).expect(0);
    assert_eq!(envelope["data"]["skipped"], 1);
    assert_eq!(envelope["data"]["entries"][0]["path"], "/rotated");
    assert_eq!(
        envelope["data"]["entries"].as_array().unwrap().len(),
        all + 1
    );
    assert_eq!(tree_hash(&home.state()), before, "net-log writes nothing");
}

#[test]
fn net_log_without_a_log_is_empty() {
    let home = Home::new(&["wow"]);
    let envelope = home.run(&["net-log", "--offline"]).expect(0);
    assert_eq!(
        (&envelope["data"]["entries"], &envelope["data"]["skipped"]),
        (&serde_json::json!([]), &Value::from(0))
    );
}
