//! `install --any-flavor` end to end (omad-author-built-loading-5po6.5): a per-addon,
//! per-client setting that installs builds the publisher did not make for the client.
//! It persists exactly where it should (the record, `update`, `sync`, `export`) and
//! clears when asked, and it never installs what the client itself refuses: a game-type
//! gate, TOCs the client never reads, retail 12's interface floor.

use crate::common;

use std::fs;

use common::addons::toc;
use common::home::Home;
use common::http::gh_release;
use serde_json::{Value, json};
use support::{FOO, addon, codes, listed, release};

#[cfg(test)]
mod support {
    use super::*;
    use common::http::Asset;

    pub const FOO: &str = "https://github.com/o/foo";

    /// `name/name.toc` with `interfaces` and `extra` lines, and a Lua file.
    pub fn addon(name: &str, interfaces: &str, extra: &[&str]) -> Vec<(String, String)> {
        vec![
            (format!("{name}/{name}.toc"), toc(interfaces, extra)),
            (format!("{name}/{name}.lua"), format!("-- {name}\n")),
        ]
    }

    /// Serves `files` as `base`'s release `tag`, its one asset `Foo.zip` listed in
    /// `release.json` with `flavors` (none: an unannotated asset).
    pub fn release(
        scenario: &std::path::Path,
        base: &str,
        tag: &str,
        flavors: &[(&str, u32)],
        files: &[(String, String)],
    ) {
        let entries: Vec<(&str, &str)> = files
            .iter()
            .map(|(p, t)| (p.as_str(), t.as_str()))
            .collect();
        let assets: [Asset<'_>; 1] = [("Foo.zip", flavors, entries)];
        gh_release(scenario, base, tag, &assets);
    }

    /// The warning codes of an envelope.
    pub fn codes(envelope: &Value) -> Vec<String> {
        envelope["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|w| w["code"].as_str().unwrap().to_owned())
            .collect()
    }

    /// `list -t <id>`'s first addon.
    pub fn listed(home: &Home, id: &str) -> Value {
        let list = home.run(&["list", "-t", id]).expect(0);
        list["data"]["targets"][0]["addons"][0].clone()
    }
}

/// A retail-only addon on Mists Classic: refused, offering the setting only among the
/// alternatives; installed with it; kept by update, export and sync; cleared by
/// `--no-any-flavor`, after which the next update refuses.
#[test]
fn the_setting_installs_a_retail_only_addon_and_persists_until_cleared() {
    let mut home = Home::new(&["wow_classic"]);
    release(
        &home.http,
        FOO,
        "v1",
        &[],
        &addon("Foo", "120105", &["## Version: 1"]),
    );

    let r = home.run(&["install", "gh:o/foo", "-t", "classic"]);
    let envelope = r.expect(3);
    let error = &envelope["error"];
    assert_eq!(error["code"], "no_compatible_release", "{}", r.context());
    let retry = json!([
        "omaddons",
        "install",
        "gh:o/foo",
        "-t",
        "classic",
        "--any-flavor"
    ]);
    let alternatives: Vec<&Value> = error["alternatives"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| &a["argv"])
        .collect();
    assert!(alternatives.contains(&&retry), "{}", r.context());
    assert!(
        !error["fix_argv"].to_string().contains("--any-flavor"),
        "never the fix: {}",
        r.context()
    );

    let r = home.run(&["install", "gh:o/foo", "-t", "classic", "--any-flavor"]);
    let envelope = r.expect(0);
    assert_eq!(
        envelope["data"]["items"][0]["status"],
        "installed",
        "{}",
        r.context()
    );
    let warned = codes(&envelope);
    assert!(
        warned.contains(&"out_of_date".to_owned()) && warned.contains(&"any_flavor".to_owned()),
        "{}",
        r.context()
    );
    assert_eq!(listed(&home, "classic")["any_flavor"], true);
    let info = home
        .run(&["info", "gh:o/foo", "-t", "classic", "--offline"])
        .expect(0);
    assert_eq!(info["data"]["installed"][0]["any_flavor"], true, "{info}");
    let status = home.run(&["status"]).expect(0);
    assert_eq!(
        status["data"]["targets"][0]["any_flavor"],
        json!(["Foo"]),
        "{status}"
    );
    let human = home.exec(&["status".into()]);
    assert!(human.stdout.contains("any flavor: Foo"), "{}", human.stdout);
    home.run(&["status", "--check"]).expect(0);

    // A newer release updates under the setting.
    let scenario = home.scenario("v2");
    release(
        &scenario,
        FOO,
        "v2",
        &[],
        &addon("Foo", "120105", &["## Version: 2"]),
    );
    let r = home.run(&["update", "-t", "classic", "--refresh"]);
    let envelope = r.expect(0);
    let item = &envelope["data"]["targets"][0]["items"][0];
    assert_eq!(item["status"], "updated", "{}", r.context());
    assert_eq!(listed(&home, "classic")["any_flavor"], true);

    // export writes it, and sync from that file installs it again.
    let exported = home.run(&["export"]).expect(0);
    let content = exported["data"]["content"].as_str().unwrap().to_owned();
    assert!(content.contains("any_flavor = true"), "{content}");
    home.run(&["remove", "Foo", "-t", "classic"]).expect(0);
    let file = home.env.root().join("exported.toml");
    fs::write(&file, &content).unwrap();
    let r = home.run(&["sync", file.to_str().unwrap()]);
    r.expect(0);
    assert_eq!(
        listed(&home, "classic")["any_flavor"],
        true,
        "{}",
        r.context()
    );

    // --no-any-flavor clears it, and the next update refuses.
    let r = home.run(&["install", "gh:o/foo", "-t", "classic", "--no-any-flavor"]);
    let envelope = r.expect(0);
    let item = &envelope["data"]["items"][0];
    assert_eq!(
        (&item["status"], &item["fields"]),
        (&json!("edited"), &json!(["any_flavor"])),
        "{}",
        r.context()
    );
    assert_eq!(listed(&home, "classic")["any_flavor"], false);
    let scenario = home.scenario("v3");
    release(
        &scenario,
        FOO,
        "v3",
        &[],
        &addon("Foo", "120105", &["## Version: 3"]),
    );
    let r = home.run(&["update", "-t", "classic", "--refresh"]);
    let envelope = r.expect(0);
    let item = &envelope["data"]["targets"][0]["items"][0];
    assert!(
        item["status"] == "skipped" || item["status"] == "failed",
        "refused without the setting: {}",
        r.context()
    );
    assert_eq!(listed(&home, "classic")["version"], "v2", "{}", r.context());
}

/// Under the setting a file whose `release.json` lists only mainline installs on Mists
/// Classic, and the warning names what it allowed.
#[test]
fn the_setting_ignores_release_json_evidence_and_names_it() {
    let home = Home::new(&["wow_classic"]);
    release(
        &home.http,
        FOO,
        "v1",
        &[("mainline", 120_105)],
        &addon("Foo", "120105, 50504", &[]),
    );
    home.run(&["install", "gh:o/foo", "-t", "classic"])
        .expect(3);
    let r = home.run(&["install", "gh:o/foo", "-t", "classic", "--any-flavor"]);
    let envelope = r.expect(0);
    let warning = envelope["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["code"] == "any_flavor")
        .cloned()
        .unwrap_or_default();
    assert!(
        warning["message"]
            .as_str()
            .is_some_and(|m| m.contains("release.json lists Foo.zip for mainline only")),
        "{}",
        r.context()
    );
}

/// What the client itself refuses stays refused under the setting: a game-type gate is
/// left out, a folder whose only TOC is for another client refuses the addon, and retail
/// 12 refuses an interface below its floor. None offers the setting.
#[test]
fn the_setting_never_installs_what_the_client_refuses() {
    let mut home = Home::new(&["wow_classic", "wow"]);
    let mut gated = addon("Foo", "120105", &[]);
    gated.extend(addon(
        "FooRetail",
        "120105",
        &["## AllowLoadGameType: standard"],
    ));
    release(&home.http, FOO, "v1", &[], &gated);
    let r = home.run(&["install", "gh:o/foo", "-t", "classic", "--any-flavor"]);
    let envelope = r.expect(0);
    let folders = &envelope["data"]["items"][0]["folders"];
    assert_eq!(folders["added"], json!(["Foo"]), "{}", r.context());
    assert_eq!(
        folders["skipped"][0]["reason"],
        "game_type_excluded",
        "{}",
        r.context()
    );

    let scenario = home.scenario("mainline-toc-only");
    release(
        &scenario,
        "https://github.com/o/bar",
        "v1",
        &[],
        &[
            ("Bar/Bar_Mainline.toc".to_owned(), toc("120105", &[])),
            ("Bar/Bar.lua".to_owned(), "-- Bar\n".to_owned()),
        ],
    );
    let r = home.run(&["install", "gh:o/bar", "-t", "classic", "--any-flavor"]);
    let error = &r.expect(3)["error"];
    assert!(
        error["details"]["tried"][0]["reason"]
            .as_str()
            .is_some_and(|t| t.contains("no_loadable_toc")),
        "{}",
        r.context()
    );

    let scenario = home.scenario("below-the-floor");
    release(
        &scenario,
        "https://github.com/o/old",
        "v1",
        &[],
        &addon("Old", "110207", &[]),
    );
    let r = home.run(&["install", "gh:o/old", "-t", "retail", "--any-flavor"]);
    let envelope = r.expect(3);
    let error = &envelope["error"];
    assert!(
        error["details"]["tried"][0]["reason"]
            .as_str()
            .is_some_and(|t| t.contains("below_client_floor")),
        "{}",
        r.context()
    );
    let r = home.run(&["install", "gh:o/old", "-t", "retail"]);
    let envelope = r.expect(3);
    assert!(
        !envelope["error"]["alternatives"]
            .to_string()
            .contains("--any-flavor"),
        "the floor is not the publisher's word: {}",
        r.context()
    );
}

/// Setting it on an installed addon is a records-only change, which undo takes back;
/// `copy` never carries it to another client.
#[test]
fn undo_takes_the_setting_back_and_copy_never_carries_it() {
    let home = Home::new(&["wow_classic", "wow_classic_era"]);
    release(
        &home.http,
        FOO,
        "v1",
        &[],
        &addon("Foo", "50504, 120105", &[]),
    );
    home.run(&["install", "gh:o/foo", "-t", "classic"])
        .expect(0);
    let r = home.run(&["install", "gh:o/foo", "-t", "classic", "--any-flavor"]);
    let envelope = r.expect(0);
    assert_eq!(
        envelope["data"]["items"][0]["fields"],
        json!(["any_flavor"]),
        "{}",
        r.context()
    );
    assert_eq!(listed(&home, "classic")["any_flavor"], true);
    home.run(&["undo"]).expect(0);
    assert_eq!(listed(&home, "classic")["any_flavor"], false);

    home.run(&["install", "gh:o/foo", "-t", "classic", "--any-flavor"])
        .expect(0);
    let r = home.run(&["copy", "--from", "classic", "--to", "era"]);
    let envelope = r.expect(0);
    let item = &envelope["data"]["targets"][0]["items"][0];
    assert_eq!(
        (&item["status"], &item["skip_reason"]),
        (&json!("skipped"), &json!("no_build_for_target")),
        "{}",
        r.context()
    );
}

/// A dependency of an addon with the setting is judged without it: a retail-only
/// library is not installed for it on Mists Classic, and is reported as today.
#[test]
fn a_dependency_is_judged_without_the_setting() {
    let mut home = Home::new(&["wow_classic"]);
    let dir = home.env.config().join("omaddons/catalog.d");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("50-libx.toml"),
        "schema_version = \"1.0\"\n\n[[addon]]\nname = \"LibX\"\nref = \"gh:o/libx\"\nfolders = [\"LibX\"]\n",
    )
    .unwrap();
    release(
        &home.http,
        FOO,
        "v1",
        &[],
        &addon("Foo", "120105", &["## Dependencies: LibX"]),
    );
    let scenario = home.scenario("with-libx");
    release(
        &scenario,
        FOO,
        "v1",
        &[],
        &addon("Foo", "120105", &["## Dependencies: LibX"]),
    );
    release(
        &scenario,
        "https://github.com/o/libx",
        "v1",
        &[],
        &addon("LibX", "120105", &[]),
    );
    let r = home.run(&["install", "gh:o/foo", "-t", "classic", "--any-flavor"]);
    let envelope = r.expect(0);
    let names: Vec<&str> = envelope["data"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["status"] == "installed")
        .filter_map(|i| i["name"].as_str())
        .collect();
    assert_eq!(names, ["Foo"], "{}", r.context());
    assert!(
        codes(&envelope)
            .iter()
            .any(|c| c == "missing_dependency" || c == "no_compatible_release"),
        "{}",
        r.context()
    );
}
