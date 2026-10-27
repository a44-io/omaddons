//! `omaddons update` (alias `upgrade`) end to end: every client's updates in one run of
//! one transaction per target with work, undone together by `omaddons undo`; skips
//! (pinned, blocked) that are not failures; the planning-time guard for adopted builds;
//! fetch failures, `--strict`, running clients, ownership fixes and usage errors.
//! Request counts come from `net.log`.

mod common;

use std::fs;

use common::home::Home;
use common::http::{fixture, gh_release, gh_release_v};
use common::netlog::net_log;
use common::trees::tree_hash;
use serde_json::{Value, json};
use support::{ALL, BAR, BAZ, EVERY, FOO, LEG, MAINLINE, PIN, QUX, Rel, items, stable};

#[cfg(test)]
mod support {
    use serde_json::Value;

    use super::common::addons::toc;
    use super::common::http::Asset;

    pub const FOO: &str = "https://github.com/o/foo";
    pub const BAR: &str = "https://github.com/o/bar";
    pub const BAZ: &str = "https://github.com/o/baz";
    pub const QUX: &str = "https://github.com/o/qux";
    pub const PIN: &str = "https://github.com/o/pin";
    pub const LEG: &str = "https://github.com/o/leg";

    /// Era, classic (Mists) and retail.
    pub const EVERY: &str = "11509, 50504, 120105";
    pub const ALL: &[(&str, u32)] = &[
        ("mainline", 120_105),
        ("classic", 11_509),
        ("mists", 50_504),
    ];
    pub const MAINLINE: &[(&str, u32)] = &[("mainline", 120_105)];

    /// A release's one asset: `<Name>.zip` with `<Name>/<Name>.toc` and a `core.lua`
    /// that differs per tag; `extra` adds folders (name, interfaces).
    pub struct Rel {
        file: String,
        flavors: &'static [(&'static str, u32)],
        entries: Vec<(String, String)>,
    }

    impl Rel {
        pub fn new(
            name: &str,
            tag: &str,
            interfaces: &str,
            flavors: &'static [(&'static str, u32)],
        ) -> Self {
            Self::with(name, tag, interfaces, flavors, &[])
        }

        pub fn with(
            name: &str,
            tag: &str,
            interfaces: &str,
            flavors: &'static [(&'static str, u32)],
            extra: &[&str],
        ) -> Self {
            let mut entries = vec![
                (
                    format!("{name}/{name}.toc"),
                    toc(
                        interfaces,
                        &[&format!("## Title: {name}"), &format!("## Version: {tag}")],
                    ),
                ),
                (format!("{name}/core.lua"), format!("-- {name} {tag}")),
            ];
            for folder in extra {
                entries.push((format!("{folder}/{folder}.toc"), toc(interfaces, &[])));
            }
            Self {
                file: format!("{name}.zip"),
                flavors,
                entries,
            }
        }

        pub fn assets(&self) -> [Asset<'_>; 1] {
            [(
                self.file.as_str(),
                self.flavors,
                self.entries
                    .iter()
                    .map(|(p, t)| (p.as_str(), t.as_str()))
                    .collect(),
            )]
        }
    }

    /// `(target, name, status, skip_reason)` per listed item.
    pub fn items(envelope: &Value) -> Vec<(String, String, String, String)> {
        let mut out = Vec::new();
        for t in envelope["data"]["targets"].as_array().unwrap() {
            for i in t["items"].as_array().unwrap() {
                out.push((
                    t["target"]["id"].as_str().unwrap().to_owned(),
                    i["name"].as_str().unwrap_or("-").to_owned(),
                    i["status"].as_str().unwrap().to_owned(),
                    i["skip_reason"].as_str().unwrap_or("-").to_owned(),
                ));
            }
        }
        out
    }

    /// The envelope without what changes between runs.
    pub fn stable(envelope: &Value) -> Value {
        let mut text = envelope.to_string();
        for t in envelope["data"]["targets"].as_array().unwrap() {
            let id = t["target"]["id"].as_str().unwrap();
            for (v, what) in [(&t["target"]["key"], "key"), (&t["tx"], "tx")] {
                if let Some(v) = v.as_str() {
                    text = text.replace(v, &format!("<{what}:{id}>"));
                }
            }
        }
        if let Some(run) = envelope["data"]["run"].as_str() {
            text = text.replace(run, "<run>");
        }
        // A dry run's token holds the build id, so it is never snapshotted raw.
        if let Some(token) = envelope["data"]["precondition"].as_str() {
            text = text.replace(token, "<precondition>");
        }
        for (key, placeholder) in [("plan_id", "<plan>"), ("plan_expires_at", "<expires>")] {
            if let Some(value) = envelope["data"][key].as_str() {
                text = text.replace(value, placeholder);
            }
        }
        let mut stable: Value = serde_json::from_str(&text).unwrap();
        stable["meta"] = Value::from("<meta>");
        stable["tool_version"] = Value::from("<version>");
        stable
    }
}

fn row(t: &str, n: &str, s: &str, r: &str) -> (String, String, String, String) {
    (t.to_owned(), n.to_owned(), s.to_owned(), r.to_owned())
}

/// Era, classic and retail with five addons: Foo behind on retail and classic, Bar
/// behind on retail, Baz current everywhere, Qux current on era.
fn three_clients() -> Home {
    let mut home = Home::new(&["wow", "wow_classic_era", "wow_classic"]);
    let (current, era_only) = (
        Rel::new("Baz", "v1", EVERY, ALL),
        Rel::new("Qux", "v1", EVERY, ALL),
    );
    gh_release(&home.http, BAZ, "v1", &current.assets());
    gh_release(&home.http, QUX, "v1", &era_only.assets());
    for target in ["retail", "era", "classic"] {
        home.run(&["install", "gh:o/baz", "-t", target]).expect(0);
    }
    home.run(&["install", "gh:o/qux", "-t", "era"]).expect(0);
    let (foo1, foo2) = (
        Rel::new("Foo", "v1", EVERY, ALL),
        Rel::new("Foo", "v2", EVERY, ALL),
    );
    home.installed_behind(
        FOO,
        ("v1", &foo1.assets()),
        ("v2", &foo2.assets()),
        &["retail", "classic"],
    );
    let (bar1, bar2) = (
        Rel::new("Bar", "v1", EVERY, ALL),
        Rel::new("Bar", "v2", EVERY, ALL),
    );
    home.installed_behind(
        BAR,
        ("v1", &bar1.assets()),
        ("v2", &bar2.assets()),
        &["retail"],
    );
    home
}

fn hashes(home: &Home) -> [String; 3] {
    ["_retail_", "_classic_", "_classic_era_"].map(|c| tree_hash(&home.addons(c)))
}

#[test]
fn updates_every_client_in_one_run_and_one_undo_reverts_it() {
    let home = three_clients();
    let before = hashes(&home);
    let log_before = net_log(&home).len();
    let r = home.run(&["update"]);
    let envelope = r.expect(0);
    assert_eq!(
        items(&envelope),
        [
            row("classic", "Foo", "updated", "-"),
            row("retail", "Bar", "updated", "-"),
            row("retail", "Foo", "updated", "-"),
        ],
        "{}",
        r.context()
    );
    let data = &envelope["data"];
    let ids: Vec<(&str, bool)> = data["targets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| (t["target"]["id"].as_str().unwrap(), t["tx"].is_string()))
        .collect();
    assert_eq!(ids, [("classic", true), ("era", false), ("retail", true)]);
    assert_eq!(
        data["summary"],
        json!({"targets": 3, "changed": 3, "unchanged": 4, "skipped": 0, "failed": 0})
    );
    let foo = &data["targets"][2]["items"][1];
    assert_eq!(
        (&foo["from_version"], &foo["to_version"]),
        (&json!("v1"), &json!("v2"))
    );
    assert_eq!(envelope["partial"], false);
    let hints: Vec<&str> = envelope["commands"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["action"].as_str().unwrap())
        .collect();
    assert_eq!(hints, ["undo_run", "undo", "undo"]);

    // One fetch per distinct ref: Foo, Bar, Baz and Qux.
    let log = net_log(&home);
    let new: Vec<_> = log.entries[log_before..].to_vec();
    let latest = |path: &str| {
        new.iter()
            .filter(|e| e.purpose == "release-latest" && e.path.starts_with(path))
            .count()
    };
    for repo in ["/o/foo/", "/o/bar/", "/o/baz/", "/o/qux/"] {
        assert!(latest(repo) <= 1, "{repo} fetched {} times", latest(repo));
    }
    common::netlog::assert_no_github_rest(&new);

    // Era had nothing to update; one undo reverts both transactions.
    let after = hashes(&home);
    assert_eq!(after[2], before[2]);
    let r = home.run(&["undo"]);
    let undo = r.expect(0);
    assert_eq!(
        undo["data"]["transactions"].as_array().unwrap().len(),
        2,
        "{}",
        r.context()
    );
    assert_eq!(hashes(&home), before);
}

#[test]
fn bare_update_is_update_all_and_upgrade_is_update() {
    let home = three_clients();
    let bare = home.run(&["update", "--dry-run"]).expect(0);
    let all = home.run(&["update", "--all", "--dry-run"]).expect(0);
    let upgrade = home.run(&["upgrade", "--dry-run"]).expect(0);
    assert_eq!(bare["data"], all["data"]);
    assert_eq!(bare["data"], upgrade["data"]);
    assert_eq!(upgrade["command"], "update");
    assert_eq!(bare["data"]["dry_run"], true);
    assert_eq!(bare["data"]["run"], Value::Null);
    assert!(bare["commands"].as_array().unwrap().is_empty());
    insta::assert_json_snapshot!("update_dry_run", stable(&bare));
    let human = home.exec(&["update".to_owned(), "--dry-run".to_owned()]);
    insta::assert_snapshot!(
        "update_dry_run_human",
        common::plans::redact_ulids(&human.stdout)
    );
}

#[test]
fn pinned_addons_are_skipped_and_naming_one_warns() {
    let mut home = Home::new(&["wow"]);
    let pin1 = Rel::new("Pin", "v1", "120105", MAINLINE);
    gh_release(&home.http, PIN, "v1", &pin1.assets());
    home.run(&["install", "gh:o/pin", "--version", "v1", "-t", "retail"])
        .expect(0);
    let scenario = home.scenario("pin-v2");
    let pin2 = Rel::new("Pin", "v2", "120105", MAINLINE);
    gh_release_v(&scenario, PIN, "v1", &pin1.assets());
    gh_release(&scenario, PIN, "v2", &pin2.assets());
    let codes = |e: &Value| -> Vec<String> {
        e["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|w| w["code"].as_str().map(str::to_owned))
            .collect()
    };
    let bare = home.run(&["update"]).expect(0);
    assert_eq!(items(&bare), [row("retail", "Pin", "skipped", "pinned")]);
    assert!(codes(&bare).is_empty(), "{:?}", codes(&bare));
    let named = home.run(&["update", "pin"]).expect(0);
    assert_eq!(items(&named), [row("retail", "Pin", "skipped", "pinned")]);
    assert_eq!(codes(&named), ["addon_pinned"]);
    assert_eq!(named["data"]["targets"][0]["tx"], Value::Null);
    let r = home.exec(&["update".to_owned()]);
    assert!(
        r.stdout
            .contains("retail: nothing to update (Pin is pinned)"),
        "{}",
        r.stdout
    );
}

#[test]
fn a_release_that_does_not_load_is_skipped_as_blocked() {
    let mut home = Home::new(&["wow", "wow_classic_era"]);
    let old = Rel::new("Foo", "v1", EVERY, ALL);
    // v2 says nothing about flavors and loads on retail only.
    let new = Rel::new("Foo", "v2", "120105", &[]);
    home.installed_behind(
        FOO,
        ("v1", &old.assets()),
        ("v2", &new.assets()),
        &["retail", "era"],
    );
    let era_before = tree_hash(&home.addons("_classic_era_"));
    let r = home.run(&["update"]);
    let envelope = r.expect(0);
    assert_eq!(
        items(&envelope),
        [
            row("era", "Foo", "skipped", "blocked"),
            row("retail", "Foo", "updated", "-")
        ],
        "{}",
        r.context()
    );
    assert_eq!(envelope["partial"], false);
    assert_eq!(tree_hash(&home.addons("_classic_era_")), era_before);
    let outdated = home.run(&["outdated", "-t", "era"]).expect(0);
    let item = &outdated["data"]["targets"][0]["items"][0];
    assert_eq!(
        (&item["status"], &item["blocked_reason"]),
        (&json!("blocked"), &json!("verified_not_loading")),
        "{outdated}"
    );
}

/// Newer releases whose TOCs a client reads with a warning: retail's is older than the
/// client (it loads only as out of date), and the 3.3.5a client's lists several
/// interfaces. The committed update says both as its dry run does.
#[test]
fn an_update_warns_what_its_new_tocs_say_as_its_dry_run_does() {
    let mut home = Home::new(&["wow"]);
    home.with_custom(common::trees::CustomRoot::at(
        home.env.root().join("wow-335"),
    ));
    let (foo1, foo2) = (
        Rel::new("Foo", "v1", "120105", MAINLINE),
        Rel::new("Foo", "v2", "120000", &[("mainline", 120_000)]),
    );
    home.installed_behind(
        FOO,
        ("v1", &foo1.assets()),
        ("v2", &foo2.assets()),
        &["retail"],
    );
    let (leg1, leg2) = (
        Rel::new("Leg", "v1", "30300", &[]),
        Rel::new("Leg", "v2", "30300, 30400", &[]),
    );
    home.installed_behind(
        LEG,
        ("v1", &leg1.assets()),
        ("v2", &leg2.assets()),
        &["wotlk"],
    );
    // Sorted: a dry run reports its targets in plan order, commits run in key order.
    let read_warnings = |envelope: &Value| -> Vec<String> {
        let mut read: Vec<String> = envelope["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|w| w["code"] == "out_of_date" || w["code"] == "toc_comma_on_legacy")
            .map(Value::to_string)
            .collect();
        read.sort();
        read
    };
    let dry = home.run(&["update", "--dry-run"]);
    let planned = read_warnings(&dry.expect(0));
    assert_eq!(planned.len(), 2, "{}", dry.context());
    for code in ["\"out_of_date\"", "\"toc_comma_on_legacy\""] {
        assert!(
            planned.iter().any(|w| w.contains(code)),
            "{code}\n{}",
            dry.context()
        );
    }
    let r = home.run(&["update"]);
    let envelope = r.expect(0);
    assert_eq!(
        items(&envelope),
        [
            row("retail", "Foo", "updated", "-"),
            row("wotlk", "Leg", "updated", "-")
        ],
        "{}",
        r.context()
    );
    assert_eq!(read_warnings(&envelope), planned, "{}", r.context());
}

/// A newer release whose TOC falls below retail 12's 120000 floor never loads there,
/// whatever the player enables: `update` leaves the installed build alone (`blocked`)
/// and `outdated` says it does not load, never that it loads out of date.
#[test]
fn a_release_below_retails_floor_is_blocked_not_installed() {
    let mut home = Home::new(&["wow"]);
    let (v1, v2) = (
        Rel::new("Foo", "v1", "120105", MAINLINE),
        Rel::new("Foo", "v2", "110207", &[]),
    );
    home.installed_behind(FOO, ("v1", &v1.assets()), ("v2", &v2.assets()), &["retail"]);
    let before = tree_hash(&home.addons("_retail_"));

    let r = home.run(&["update"]);
    let envelope = r.expect(0);
    assert_eq!(
        items(&envelope),
        [row("retail", "Foo", "skipped", "blocked")],
        "{}",
        r.context()
    );
    assert!(
        !envelope.to_string().contains("out_of_date"),
        "{}",
        r.context()
    );
    assert_eq!(tree_hash(&home.addons("_retail_")), before);
    let r = home.run(&["outdated", "-t", "retail"]);
    let outdated = r.expect(0);
    let item = &outdated["data"]["targets"][0]["items"][0];
    assert_eq!(
        (&item["status"], &item["blocked_reason"]),
        (&json!("blocked"), &json!("verified_not_loading")),
        "{}",
        r.context()
    );
    assert!(
        !outdated.to_string().contains("Load out of date"),
        "{}",
        r.context()
    );
}

#[test]
fn an_adopted_build_is_never_replaced_by_an_older_one() {
    let mut home = Home::new(&["wow"]);
    let old = Rel::new("Foo", "v1", "120105", MAINLINE);
    // Newer by tag, but built for an older interface: out of date on this client.
    let new = Rel::new("Foo", "v2", "120000", &[]);
    home.installed_behind(
        FOO,
        ("v1", &old.assets()),
        ("v2", &new.assets()),
        &["retail"],
    );
    // As an identified adoption would record it: versioned by its TOC.
    let paths = common::http::paths(&home.env);
    let key = common::state::key_of(&home, "retail");
    let mut record = common::state::record(&home, "retail");
    for a in record.addons.values_mut() {
        a.version_confidence = omaddons::model::VersionConfidence::Toc;
    }
    fs::write(
        paths.target_record(&key),
        omaddons::state::store::encode(&record).unwrap(),
    )
    .unwrap();
    let before = tree_hash(&home.addons("_retail_"));

    let r = home.run(&["update"]);
    let envelope = r.expect(0);
    assert_eq!(
        items(&envelope),
        [row("retail", "Foo", "skipped", "blocked")],
        "{}",
        r.context()
    );
    assert_eq!(envelope["partial"], false);
    assert_eq!(
        tree_hash(&home.addons("_retail_")),
        before,
        "the folder is untouched"
    );
    let (memo, _) = omaddons::state::memo::load(&paths);
    assert!(
        memo.candidates
            .iter()
            .any(|f| f.release_id == "gh:o/foo@v2")
    );
}

#[test]
fn a_failed_fetch_fails_its_items_and_strict_exits_6() {
    let mut home = Home::new(&["wow"]);
    let (foo, bar) = (
        Rel::new("Foo", "v1", "120105", MAINLINE),
        Rel::new("Bar", "v1", "120105", MAINLINE),
    );
    gh_release(&home.http, FOO, "v1", &foo.assets());
    gh_release(&home.http, BAR, "v1", &bar.assets());
    home.run(&["install", "gh:o/foo", "gh:o/bar", "-t", "retail"])
        .expect(0);
    // A day later, Foo's source answers 503; Bar's is still at v1.
    let paths = common::http::paths(&home.env);
    let (mut memo, _) = omaddons::state::memo::load(&paths);
    for entry in memo.entries.values_mut() {
        entry.fetched_at -= jiff::SignedDuration::from_hours(24);
    }
    home.seed_memo(&memo);
    let broken = home.scenario("broken");
    gh_release(&broken, BAR, "v1", &bar.assets());
    let url = format!("{FOO}/releases/latest");
    fixture(
        &broken,
        "foo-latest-503",
        &url,
        503,
        &[("retry-after", "0")],
        None,
    );

    let r = home.run(&["update", "-v"]);
    let envelope = r.expect(0);
    assert_eq!(
        items(&envelope),
        [
            row("retail", "Bar", "unchanged", "-"),
            row("retail", "Foo", "failed", "-")
        ],
        "{}",
        r.context()
    );
    assert_eq!(envelope["partial"], true);
    assert_eq!(
        envelope["data"]["targets"][0]["items"][1]["error"]["code"],
        "http_status"
    );
    let strict = home.run(&["update", "--strict"]);
    let failed = strict.expect(6);
    assert_eq!(
        failed["error"]["code"],
        "check_failed",
        "{}",
        strict.context()
    );
    assert_eq!(failed["data"]["summary"]["failed"], 1);
}

#[test]
fn running_clients_follow_the_running_policy() {
    let mut home = Home::new(&["wow"]);
    let custom = |home: &mut Home, dir: &str, version: (u16, u16, u16, u16)| {
        let (major, minor, patch, build) = version;
        home.with_custom(
            common::trees::CustomRoot::at(home.env.root().join(dir))
                .version(major, minor, patch, build),
        )
    };
    let legacy = custom(&mut home, "wow-335", (3, 3, 5, 12_340));
    let vanilla_client = custom(&mut home, "wow-112", (1, 12, 1, 5875));
    let targets = home.run(&["targets"]).expect(0);
    let id_of = |family: &str| -> String {
        targets["data"]["targets"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["family"] == family)
            .and_then(|t| t["id"].as_str())
            .unwrap()
            .to_owned()
    };
    let (wotlk, vanilla) = (id_of("wrath-legacy"), id_of("vanilla-legacy"));
    let (foo1, foo2) = (
        Rel::new("Foo", "v1", "120105", MAINLINE),
        Rel::new("Foo", "v2", "120105", MAINLINE),
    );
    home.installed_behind(
        FOO,
        ("v1", &foo1.assets()),
        ("v2", &foo2.assets()),
        &["retail"],
    );
    let (leg1, leg2) = (
        Rel::new("Leg", "v1", "30300", &[]),
        Rel::new("Leg", "v2", "30300", &[]),
    );
    home.installed_behind(
        LEG,
        ("v1", &leg1.assets()),
        ("v2", &leg2.assets()),
        &[&wotlk],
    );
    let (qux1, qux2) = (
        Rel::new("Qux", "v1", "11200", &[]),
        Rel::new("Qux", "v2", "11200", &[]),
    );
    home.installed_behind(
        QUX,
        ("v1", &qux1.assets()),
        ("v2", &qux2.assets()),
        &[&vanilla],
    );

    // Every client runs: retail in its Wine prefix, the legacy clients under wine.
    let prefix = home.prefix.display().to_string();
    let exe = |client: &std::path::Path| client.join("Wow.exe").display().to_string();
    let (wine335, wine112) = (exe(&legacy), exe(&vanilla_client));
    let proc = common::procfs::FakeProc::new(home.env.root().join("proc"))
        .process(
            4242,
            true,
            "Wow.exe",
            &["C:\\Program Files (x86)\\World of Warcraft\\_retail_\\Wow.exe"],
            &[("WINEPREFIX", &prefix)],
        )
        .process(9, true, "Wow.exe", &["/usr/bin/wine", &wine335], &[])
        .process(10, true, "Wow.exe", &["/usr/bin/wine", &wine112], &[]);
    home.proc = Some(proc.root().to_path_buf());

    // Retail and 3.3.5a re-read a loaded addon's TOC on /reload, so their replacements
    // land; 1.12.1 needs a restart for any change.
    let r = home.run(&["update"]);
    let envelope = r.expect(0);
    assert_eq!(envelope["partial"], true, "{}", r.context());
    for t in envelope["data"]["targets"].as_array().unwrap() {
        if t["target"]["id"] == vanilla.as_str() {
            assert_eq!(t["tx"], Value::Null);
            let error = &t["items"][0]["error"];
            assert_eq!(
                (&error["code"], &error["details"]["reason"]),
                (&json!("game_running"), &json!("legacy_client")),
                "{t}"
            );
        } else {
            assert!(t["tx"].is_string(), "{t}");
            assert_eq!(t["reload_required"], "reload", "{t}");
        }
    }
}

#[test]
fn undoing_one_target_restores_it_byte_for_byte() {
    let mut home = Home::new(&["wow", "wow_classic_era"]);
    let (old, new) = (
        Rel::new("Foo", "v1", EVERY, ALL),
        Rel::new("Foo", "v2", EVERY, ALL),
    );
    home.installed_behind(
        FOO,
        ("v1", &old.assets()),
        ("v2", &new.assets()),
        &["retail", "era"],
    );
    let retail_before = tree_hash(&home.addons("_retail_"));
    let envelope = home.run(&["update"]).expect(0);
    let era_after = tree_hash(&home.addons("_classic_era_"));
    let retail_tx = envelope["data"]["targets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["target"]["id"] == "retail")
        .and_then(|t| t["tx"].as_str())
        .unwrap()
        .to_owned();
    home.run(&["undo", &retail_tx]).expect(0);
    assert_eq!(tree_hash(&home.addons("_retail_")), retail_before);
    assert_eq!(tree_hash(&home.addons("_classic_era_")), era_after);
}

#[test]
fn an_ownership_conflict_names_install_force() {
    let mut home = Home::new(&["wow"]);
    let bar = Rel::with("Bar", "v1", "120105", MAINLINE, &["Shared"]);
    gh_release(&home.http, BAR, "v1", &bar.assets());
    home.run(&["install", "gh:o/bar", "-t", "retail"]).expect(0);
    let (old, new) = (
        Rel::new("Foo", "v1", "120105", MAINLINE),
        Rel::with("Foo", "v2", "120105", MAINLINE, &["Shared"]),
    );
    home.installed_behind(
        FOO,
        ("v1", &old.assets()),
        ("v2", &new.assets()),
        &["retail"],
    );
    let r = home.run(&["update", "foo"]);
    let envelope = r.expect(4);
    assert_eq!(
        envelope["error"]["code"],
        "ownership_conflict",
        "{}",
        r.context()
    );
    assert_eq!(
        envelope["error"]["fix_argv"],
        json!(["omaddons", "install", "Foo", "--force", "-t", "retail"])
    );
}

#[test]
fn usage_errors_and_names_installed_nowhere() {
    let home = Home::new(&["wow"]);
    let foo = Rel::new("Foo", "v1", "120105", MAINLINE);
    gh_release(&home.http, FOO, "v1", &foo.assets());
    home.run(&["install", "gh:o/foo", "-t", "retail"]).expect(0);

    let all = home.run(&["update", "foo", "--all"]).expect(2);
    assert_eq!(
        all["error"]["fix_argv"],
        json!(["omaddons", "update", "foo"])
    );
    let channel = home.run(&["update", "--channel", "beta"]).expect(2);
    assert_eq!(
        (&channel["error"]["fix"], &channel["error"]["fix_argv"]),
        (
            &json!("omaddons update <addon> --channel beta"),
            &Value::Null
        )
    );
    let version = home.run(&["update", "foo", "--version", "v1"]).expect(2);
    assert_eq!(
        version["error"]["fix_argv"],
        json!(["omaddons", "install", "foo", "--version", "v1"])
    );
    let nowhere = home.run(&["update", "nosuch"]).expect(3);
    assert_eq!(
        (&nowhere["error"]["code"], &nowhere["error"]["fix_argv"]),
        (
            &json!("not_managed"),
            &json!(["omaddons", "install", "nosuch"])
        )
    );
    // No client at all: still `not_managed`, never "nothing to update".
    let empty = Home::new(&[]);
    let none = empty.run(&["update", "nosuch"]).expect(3);
    assert_eq!(none["error"]["code"], "not_managed", "{none}");
    let mixed = home.run(&["update", "foo", "nosuch", "-v"]).expect(0);
    assert_eq!(mixed["partial"], true);
    assert!(items(&mixed).contains(&row("retail", "-", "failed", "-")));

    let help = home.exec(&["help".to_owned(), "update".to_owned()]);
    assert_eq!(help.code, 0, "{}", help.context());
    assert!(
        help.stdout.contains("install <addon> --version"),
        "{}",
        help.stdout
    );
}

#[test]
fn channel_switches_install_that_channels_newest() {
    let mut home = Home::new(&["wow"]);
    let foo = Rel::new("Foo", "v1", "120105", MAINLINE);
    gh_release(&home.http, FOO, "v1", &foo.assets());
    home.run(&["install", "gh:o/foo", "-t", "retail"]).expect(0);
    // The API lists a newer prerelease; its zip is the beta build.
    let scenario = home.scenario("beta");
    let beta = Rel::new("Foo", "v2-beta", "120105", &[]);
    gh_release(&scenario, FOO, "v1", &foo.assets());
    gh_release_v(&scenario, FOO, "v2-beta", &beta.assets());
    let list = json!([
        {"tag_name": "v2-beta", "prerelease": true, "draft": false,
         "published_at": "2026-10-15T00:00:00Z",
         "assets": [{"name": "Foo.zip",
                     "browser_download_url": format!("{FOO}/releases/download/v2-beta/Foo.zip")}]},
        {"tag_name": "v1", "prerelease": false, "draft": false,
         "published_at": "2026-10-01T00:00:00Z", "assets": []}
    ])
    .to_string();
    fixture(
        &scenario,
        "api-releases",
        "https://api.github.com/repos/o/foo/releases?per_page=20",
        200,
        &[("content-type", "application/json")],
        Some(&list),
    );
    let r = home.run(&["update", "foo", "--channel", "beta"]);
    let envelope = r.expect(0);
    assert_eq!(
        items(&envelope),
        [row("retail", "Foo", "updated", "-")],
        "{}",
        r.context()
    );
    let record = common::state::record(&home, "retail");
    let foo = &record.addons["foo"];
    assert_eq!(
        (foo.channel, foo.version.as_str()),
        (omaddons::model::Channel::Beta, "v2-beta")
    );
}
