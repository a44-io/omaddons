//! `omaddons pin` and `omaddons unpin` end to end: records-only transactions of kind
//! `pin` (undoable, no game file touched, fine while the client runs), `pin --version`
//! through the install pipeline, no-ops, fan-out across clients, carry-over of reason
//! and channel, the offline hint after `unpin`, and `addon_pinned`'s `unpin` fix.

use crate::common;

use std::fs;

use common::home::Home;
use common::http::gh_release;
use common::netlog::net_log;
use common::trees::{CustomRoot, tree_hash};
use serde_json::{Value, json};
use support::{FOO, Rel, items, last_history, stable};

#[cfg(test)]
mod support {
    use serde_json::Value;

    use super::common::addons::toc;
    use super::common::http::Asset;

    pub const FOO: &str = "https://github.com/o/foo";
    pub const MAINLINE: &[(&str, u32)] = &[("mainline", 120_105)];

    /// A release's one asset, `Foo.zip`: one TOC per (file suffix, interface), so each
    /// client reads its own, and a `core.lua` that differs per tag.
    pub struct Rel {
        flavors: &'static [(&'static str, u32)],
        entries: Vec<(String, String)>,
    }

    impl Rel {
        pub fn new(
            tag: &str,
            tocs: &[(&str, &str)],
            flavors: &'static [(&'static str, u32)],
        ) -> Self {
            let mut entries: Vec<(String, String)> = tocs
                .iter()
                .map(|(suffix, interface)| {
                    (
                        format!("Foo/Foo{suffix}.toc"),
                        toc(interface, &["## Title: Foo", &format!("## Version: {tag}")]),
                    )
                })
                .collect();
            entries.push(("Foo/core.lua".into(), format!("-- Foo {tag}")));
            Self { flavors, entries }
        }

        pub fn retail(tag: &str) -> Self {
            Self::new(tag, &[("", "120105")], MAINLINE)
        }

        pub fn assets(&self) -> [Asset<'_>; 1] {
            [(
                "Foo.zip",
                self.flavors,
                self.entries
                    .iter()
                    .map(|(p, t)| (p.as_str(), t.as_str()))
                    .collect(),
            )]
        }
    }

    /// `(target, status)` per listed item.
    pub fn items(envelope: &Value) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for t in envelope["data"]["targets"].as_array().unwrap() {
            for i in t["items"].as_array().unwrap() {
                out.push((
                    t["target"]["id"].as_str().unwrap().to_owned(),
                    i["status"].as_str().unwrap().to_owned(),
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

    /// The newest history line.
    pub fn last_history(home: &super::Home) -> omaddons::state::history::HistoryEntry {
        super::common::state::history(home).pop().unwrap()
    }
}

fn pair(target: &str, status: &str) -> (String, String) {
    (target.to_owned(), status.to_owned())
}

/// Retail with Foo v1 installed while v2 is out.
fn behind() -> Home {
    let mut home = Home::new(&["wow"]);
    let (v1, v2) = (Rel::retail("v1"), Rel::retail("v2"));
    home.installed_behind(FOO, ("v1", &v1.assets()), ("v2", &v2.assets()), &["retail"]);
    home
}

#[test]
fn a_pin_is_a_records_only_transaction_that_update_respects_and_undo_reverts() {
    let mut home = behind();
    home.retail_running();
    let addons = tree_hash(&home.addons("_retail_"));
    let dry = home.exec(&["pin".to_owned(), "foo".to_owned(), "--dry-run".to_owned()]);
    insta::assert_snapshot!(
        "pin_one_target_human",
        common::plans::redact_ulids(&dry.stdout)
    );
    let r = home.run(&["pin", "foo"]);
    let envelope = r.expect(0);
    assert_eq!(
        items(&envelope),
        [pair("retail", "pinned")],
        "{}",
        r.context()
    );
    assert_eq!(envelope["data"]["targets"][0]["kind"], "pin");
    assert_eq!(
        tree_hash(&home.addons("_retail_")),
        addons,
        "no game file touched"
    );
    let line = last_history(&home);
    assert_eq!(line.kind, omaddons::model::TxKind::Pin);
    assert!(!line.ops.is_empty());
    assert!(
        line.ops
            .iter()
            .all(|op| op.op == omaddons::state::history::FolderOp::Record)
    );
    insta::assert_json_snapshot!("pin_one_target", stable(&envelope));
    assert!(common::state::record(&home, "retail").addons["foo"].pinned);

    home.nothing_running();
    let update = home.run(&["update"]).expect(0);
    let item = &update["data"]["targets"][0]["items"][0];
    assert_eq!(
        (&item["status"], &item["skip_reason"]),
        (&json!("skipped"), &json!("pinned"))
    );
    home.run(&["undo"]).expect(0);
    assert!(!common::state::record(&home, "retail").addons["foo"].pinned);
}

#[test]
fn pin_version_installs_and_pins_or_only_records_the_pin() {
    let home = behind();
    let r = home.run(&["pin", "foo", "--version", "v2"]);
    let moved = r.expect(0);
    let target = &moved["data"]["targets"][0];
    assert_eq!(items(&moved), [pair("retail", "pinned")], "{}", r.context());
    assert_eq!(target["kind"], "update");
    assert_eq!(
        (
            &target["items"][0]["from_version"],
            &target["items"][0]["to_version"]
        ),
        (&json!("v1"), &json!("v2"))
    );
    let record = common::state::record(&home, "retail");
    assert_eq!(
        (
            record.addons["foo"].pinned,
            record.addons["foo"].version.as_str()
        ),
        (true, "v2")
    );

    // Already there and pinned: nothing to do.
    let again = home.run(&["pin", "foo", "--version", "v2", "-v"]).expect(0);
    assert_eq!(items(&again), [pair("retail", "unchanged")]);
    assert_eq!(again["data"]["targets"][0]["tx"], Value::Null);

    // There but unpinned: only the record changes.
    home.run(&["unpin", "foo"]).expect(0);
    let addons = tree_hash(&home.addons("_retail_"));
    let records_only = home.run(&["pin", "foo", "--version", "v2"]).expect(0);
    assert_eq!(records_only["data"]["targets"][0]["kind"], "pin");
    assert_eq!(tree_hash(&home.addons("_retail_")), addons);
}

/// A dependency the pinned version brings in is installed, not pinned: only what the
/// command named is relabelled.
#[test]
fn a_dependency_pin_version_brings_in_is_not_reported_pinned() {
    let mut home = Home::new(&["wow"]);
    let v1 = Rel::retail("v1");
    let toc = common::addons::toc("120105", &["## Title: Foo", "## Dependencies: LibFoo"]);
    let v2 = [(
        "Foo.zip",
        support::MAINLINE,
        vec![("Foo/Foo.toc", toc.as_str())],
    )];
    home.installed_behind(FOO, ("v1", &v1.assets()), ("v2", &v2), &["retail"]);
    let dir = home.env.config().join("omaddons/catalog.d");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("50-lib.toml"),
        "schema_version = \"1.0\"\n\n[[addon]]\nname = \"LibFoo\"\nref = \"gh:o/libfoo\"\nfolders = [\"LibFoo\"]\n",
    )
    .unwrap();
    let lib = common::addons::toc("120105", &["## Title: LibFoo"]);
    let assets = [(
        "LibFoo.zip",
        support::MAINLINE,
        vec![("LibFoo/LibFoo.toc", lib.as_str())],
    )];
    gh_release(&home.http, "https://github.com/o/libfoo", "v1", &assets);

    let r = home.run(&["pin", "foo", "--version", "v2"]);
    let envelope = r.expect(0);
    let mut statuses = items(&envelope);
    statuses.sort();
    assert_eq!(
        statuses,
        [pair("retail", "installed"), pair("retail", "pinned")],
        "{}",
        r.context()
    );
    let record = common::state::record(&home, "retail");
    assert!(record.addons["foo"].pinned && !record.addons["libfoo"].pinned);
}

#[test]
fn pinning_a_pinned_addon_or_unpinning_an_unpinned_one_does_nothing() {
    let home = behind();
    let lines = common::state::history(&home).len();
    let unpin = home.run(&["unpin", "foo", "-v"]).expect(0);
    assert_eq!(items(&unpin), [pair("retail", "unchanged")]);
    assert_eq!(unpin["data"]["targets"][0]["tx"], Value::Null);
    home.run(&["pin", "foo"]).expect(0);
    let pin = home.run(&["pin", "foo"]).expect(0);
    assert_eq!(pin["data"]["summary"]["unchanged"], 1);
    assert!(items(&pin).is_empty(), "unchanged is listed with -v only");
    assert_eq!(common::state::history(&home).len(), lines + 1);
}

#[test]
fn pins_fan_out_to_the_clients_that_have_the_addon() {
    let mut home = Home::new(&["wow", "wow_classic_era"]);
    // A second vanilla client: a private server running the 1.15 client.
    let custom =
        home.with_custom(CustomRoot::at(home.env.root().join("wow-115")).version(1, 15, 9, 64_520));
    let targets = home.run(&["targets"]).expect(0);
    let default_id = targets["data"]["targets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| {
            t["client_dir"]
                .as_str()
                .is_some_and(|d| std::path::Path::new(d) == custom)
        })
        .and_then(|t| t["id"].as_str())
        .unwrap()
        .to_owned();
    // Its default id (`vanilla`) would shadow the family in `-t vanilla`.
    let custom_id = "turtle".to_owned();
    home.run(&["targets", "rename", &default_id, &custom_id])
        .expect(0);
    // One TOC per family: vanilla clients read Foo_Vanilla.toc, retail Foo_Mainline.toc.
    let v1 = Rel::new("v1", &[("_Vanilla", "11509"), ("_Mainline", "120105")], &[]);
    gh_release(&home.http, FOO, "v1", &v1.assets());
    for target in ["retail", "era", custom_id.as_str()] {
        let r = home.run(&["install", "gh:o/foo", "-t", target]);
        r.expect(0);
    }

    // -t vanilla: both vanilla clients, not retail; one run, one undo for both.
    let r = home.run(&["pin", "foo", "-t", "vanilla"]);
    let pinned = r.expect(0);
    let mut expected = vec![pair("era", "pinned"), pair(&custom_id, "pinned")];
    expected.sort();
    assert_eq!(items(&pinned), expected, "{}", r.context());
    assert_eq!(pinned["commands"][0]["action"], "undo_run");
    let lines = common::state::history(&home);
    let runs: Vec<_> = lines.iter().rev().take(2).map(|l| l.run).collect();
    assert!(runs[0].is_some() && runs[0] == runs[1]);
    assert!(!common::state::record(&home, "retail").addons["foo"].pinned);

    // No -t: every client that has it; only retail still needs the pin.
    let all = home.run(&["pin", "foo", "-v"]).expect(0);
    assert_eq!(all["data"]["summary"]["changed"], 1);
    let dry = home.exec(&["unpin".to_owned(), "foo".to_owned(), "--dry-run".to_owned()]);
    insta::assert_snapshot!(
        "unpin_three_targets_human",
        common::plans::redact_ulids(&dry.stdout)
    );
    let unpinned = home.run(&["unpin", "foo"]).expect(0);
    assert_eq!(unpinned["data"]["summary"]["changed"], 3);
    insta::assert_json_snapshot!("unpin_three_targets", stable(&unpinned));
}

#[test]
fn an_addon_installed_nowhere_and_version_on_a_file_install() {
    let home = behind();
    let r = home.run(&["pin", "nosuch"]);
    let nowhere = r.expect(3);
    assert_eq!(
        (&nowhere["error"]["code"], &nowhere["error"]["fix_argv"]),
        (
            &json!("not_managed"),
            &json!(["omaddons", "install", "nosuch"])
        ),
        "{}",
        r.context()
    );
    home.run(&["unpin", "nosuch"]).expect(3);

    let dl = home.env.root().join("dl");
    fs::create_dir_all(&dl).unwrap();
    let zip = dl.join("Loc.zip");
    fs::write(
        &zip,
        common::archives::zip_text(&[("Loc/Loc.toc", "## Interface: 120105\n")]),
    )
    .unwrap();
    home.run(&["install", &zip.display().to_string(), "-t", "retail"])
        .expect(0);
    let usage = home.run(&["pin", "loc", "--version", "v1"]).expect(2);
    assert_eq!(
        usage["error"]["fix_argv"],
        json!(["omaddons", "pin", "loc"])
    );
    home.run(&["pin", "loc"]).expect(0);
}

#[test]
fn pins_keep_the_reason_and_the_channel() {
    let home = behind();
    let paths = common::http::paths(&home.env);
    let key = common::state::key_of(&home, "retail");
    let mut record = common::state::record(&home, "retail");
    for a in record.addons.values_mut() {
        a.reason = omaddons::model::AddonReason::Dependency;
        a.channel = omaddons::model::Channel::Beta;
    }
    fs::write(
        paths.target_record(&key),
        omaddons::state::store::encode(&record).unwrap(),
    )
    .unwrap();
    home.run(&["pin", "foo"]).expect(0);
    let foo = &common::state::record(&home, "retail").addons["foo"];
    assert_eq!(
        (foo.pinned, foo.reason, foo.channel),
        (
            true,
            omaddons::model::AddonReason::Dependency,
            omaddons::model::Channel::Beta
        )
    );
    home.run(&["pin", "foo", "--version", "v2"]).expect(0);
    let foo = &common::state::record(&home, "retail").addons["foo"];
    assert_eq!(
        (foo.version.as_str(), foo.reason, foo.channel),
        (
            "v2",
            omaddons::model::AddonReason::Dependency,
            omaddons::model::Channel::Beta
        )
    );
}

#[test]
fn unpin_says_what_update_would_install_without_a_request() {
    let home = behind();
    home.run(&["outdated"]).expect(0);
    home.run(&["pin", "foo"]).expect(0);
    let before = net_log(&home).len();
    let r = home.exec(&["unpin".to_owned(), "foo".to_owned()]);
    assert_eq!(r.code, 0, "{}", r.context());
    assert!(
        r.stdout
            .contains("retail: `omaddons update Foo -t retail` installs v2"),
        "{}",
        r.stdout
    );
    assert_eq!(net_log(&home).len(), before);
}

#[test]
fn install_on_a_pinned_addon_names_unpin_and_unpin_runs() {
    let home = behind();
    home.run(&["pin", "foo"]).expect(0);
    let r = home.run(&["install", "gh:o/foo", "-t", "retail"]);
    let envelope = r.expect(0);
    let warning = envelope["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["code"] == "addon_pinned")
        .unwrap()
        .clone();
    assert_eq!(
        warning["fix_argv"],
        json!(["omaddons", "unpin", "Foo", "-t", "retail"])
    );
    let argv: Vec<&str> = warning["fix_argv"].as_array().unwrap()[1..]
        .iter()
        .map(|a| a.as_str().unwrap())
        .collect();
    let unpinned = home.run(&argv).expect(0);
    assert_eq!(items(&unpinned), [pair("retail", "unpinned")]);
    let help = home.exec(&["help".to_owned(), "update".to_owned()]);
    assert!(
        help.stdout.contains("omaddons pin <addon> --version"),
        "{}",
        help.stdout
    );
    let version = home.run(&["update", "foo", "--version", "v1"]).expect(2);
    assert_eq!(
        version["error"]["alternatives"][0]["argv"],
        json!(["omaddons", "pin", "foo", "--version", "v1"])
    );
}
