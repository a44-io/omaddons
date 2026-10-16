//! `omaddons export` end to end: records from fixture installs become one `addons.toml`
//! (family sections for what every live client of a family has alike, client sections
//! for the rest), printed by default and written with `--write`, which keeps a backup
//! and refuses hand edits.

mod common;

use std::fs;
use std::path::Path;

use common::archives::zip_text;
use common::home::Home;
use common::http::{gh_release, paths};
use common::netlog::net_log;
use common::trees::{CustomRoot, tree_hash};
use omaddons::model::AddonReason;
use omaddons::state::desired::{DesiredState, parse};
use serde_json::{Value, json};
use support::{content, layout, release, retail_home};

#[cfg(test)]
mod support {
    use super::*;

    const MAINLINE: &[(&str, u32)] = &[("mainline", 120_105)];

    /// Serves `name` (one folder of that name, for retail and the PTR) as `gh:o/<name>`
    /// release `v1`.
    pub fn release(home: &Home, name: &str) {
        let base = format!("https://github.com/o/{}", name.to_ascii_lowercase());
        let toc = format!("## Interface: 120100, 120105\n## Title: {name}\n");
        let path = format!("{name}/{name}.toc");
        let assets = [("a.zip", MAINLINE, vec![(path.as_str(), toc.as_str())])];
        gh_release(&home.http, &base, "v1", &assets);
    }

    /// Retail with `addons` released and installed.
    pub fn retail_home(addons: &[&str]) -> Home {
        let home = Home::new(&["wow"]);
        for name in addons {
            release(&home, name);
            let r = format!("gh:o/{}", name.to_ascii_lowercase());
            home.run(&["install", &r, "-t", "retail"]).expect(0);
        }
        home
    }

    /// The document of an `export --json` envelope, parsed back: it must parse with no
    /// warning.
    pub fn content(envelope: &Value) -> (String, DesiredState) {
        let text = envelope["data"]["content"].as_str().unwrap().to_owned();
        let (state, warnings) = parse(&text, Path::new("addons.toml")).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        (text, state)
    }

    /// `section → [name]`, family sections first.
    pub fn layout(state: &DesiredState) -> Vec<(String, Vec<String>)> {
        let names = |s: &omaddons::state::desired::Section| -> Vec<String> {
            let mut names: Vec<String> = s.entries.iter().filter_map(|e| e.name.clone()).collect();
            names.sort();
            names
        };
        state
            .families
            .iter()
            .map(|(f, s)| (format!("family.{}", f.as_str()), names(s)))
            .chain(state.targets.iter().map(|(id, s)| (id.clone(), names(s))))
            .collect()
    }
}

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| (*x).to_owned()).collect()
}

#[test]
fn what_every_mainline_client_has_alike_is_the_family_section() {
    let mut home = Home::new(&["wow", "wowt"]);
    let steam = home
        .with_custom(CustomRoot::at(home.env.root().join("steam-wow")).version(12, 1, 5, 64_000));
    let targets = home.run(&["targets"]).expect(0);
    let ids: Vec<String> = targets["data"]["targets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["id"].as_str().unwrap().to_owned())
        .collect();
    let found = ids
        .iter()
        .find(|id| *id != "retail" && *id != "ptr")
        .unwrap()
        .clone();
    assert!(ids.contains(&"ptr".to_owned()), "{ids:?}");
    let custom = "steam".to_owned();
    home.run(&["targets", "rename", &found, &custom]).expect(0);
    for name in ["DBM", "Plater", "Foo", "Lib"] {
        release(&home, name);
    }
    let install = |r: &str, t: &str| home.run(&["install", r, "-t", t]).expect(0);
    for t in ["retail", custom.as_str()] {
        install("gh:o/dbm", t);
        install("gh:o/plater", t);
    }
    install("gh:o/foo", "retail");
    install("gh:o/lib", "retail");
    install("gh:o/dbm", "ptr");
    home.run(&["pin", "DBM"]).expect(0);
    // Lib as Foo's dependency on retail.
    let key = common::state::key_of(&home, "retail");
    let mut record = common::state::record(&home, "retail");
    let lib = record.addons.get_mut("lib").unwrap();
    lib.reason = AddonReason::Dependency;
    lib.required_by = vec!["Foo".into()];
    fs::write(
        paths(&home.env).target_record(&key),
        omaddons::state::store::encode(&record).unwrap(),
    )
    .unwrap();
    // A local zip is machine-specific: left out, with a warning.
    let dl = home.env.root().join("dl");
    fs::create_dir_all(&dl).unwrap();
    let zip = dl.join("Loc.zip");
    fs::write(&zip, zip_text(&[("Loc/Loc.toc", "## Interface: 120105\n")])).unwrap();
    home.run(&["install", &zip.display().to_string(), "-t", "retail"])
        .expect(0);
    let _ = steam;

    let requests = net_log(&home).len();
    let r = home.run(&["export"]);
    let envelope = r.expect(0);
    let (text, state) = content(&envelope);
    assert_eq!(
        layout(&state),
        [
            ("family.mainline".to_owned(), s(&["DBM", "Plater"])),
            (custom.clone(), Vec::new()),
            ("ptr".to_owned(), s(&["DBM"])),
            ("retail".to_owned(), s(&["Foo", "Lib"])),
        ]
        .into_iter()
        .filter(|(_, names)| !names.is_empty())
        .collect::<Vec<_>>(),
        "{}",
        r.context()
    );
    let family = &state.families[&omaddons::rules::Family::Mainline].entries;
    let dbm = family
        .iter()
        .find(|e| e.name.as_deref() == Some("DBM"))
        .unwrap();
    assert_eq!(
        (dbm.r#ref.as_deref(), dbm.version.as_deref(), dbm.pinned),
        (Some("gh:o/dbm"), Some("v1"), true)
    );
    assert!(dbm.sha256.is_some(), "the archive is in the blob cache");
    let lib = state.targets["retail"]
        .entries
        .iter()
        .find(|e| e.name.as_deref() == Some("Lib"))
        .unwrap();
    assert!(lib.dependency && lib.version.as_deref() == Some("v1"));
    assert!(
        !text.contains("channel"),
        "the default channel is not written"
    );
    assert!(
        envelope["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w["code"] == "export_local_source"),
        "{}",
        r.context()
    );
    assert_eq!(
        envelope["data"]["targets"],
        json!(["ptr", "retail", custom.clone()])
    );
    assert_eq!(envelope["data"]["entries"], 5);
    assert_eq!(
        (&envelope["data"]["path"], &envelope["data"]["backup"]),
        (&Value::Null, &Value::Null)
    );

    insta::assert_snapshot!("export_document", text);

    // Human mode prints the document alone; repeated runs are identical; no request.
    let human = home.exec(&s(&["export"]));
    assert_eq!(human.code, 0, "{}", human.context());
    assert_eq!(human.stdout, text);
    let again = home.exec(&s(&["export"]));
    assert_eq!(again.stdout, human.stdout, "deterministic");
    assert_eq!(net_log(&home).len(), requests, "no request");
}

#[test]
fn export_writes_nothing_without_write() {
    let home = retail_home(&["Foo"]);
    let state = tree_hash(&home.state());
    let config = tree_hash(&home.env.config());
    home.run(&["export"]).expect(0);
    assert_eq!(tree_hash(&home.state()), state);
    assert_eq!(tree_hash(&home.env.config()), config);
    assert!(!home.env.cache().join("omaddons/export.json").exists());
}

#[test]
fn write_keeps_the_previous_file_and_refuses_hand_edits() {
    let home = retail_home(&["Foo"]);
    let file = home.env.config().join("omaddons/addons.toml");
    let bak = home.env.config().join("omaddons/addons.toml.bak");

    let r = home.run(&["export", "--write"]);
    let first = r.expect(0);
    assert_eq!(
        (&first["data"]["path"], &first["data"]["backup"]),
        (&json!(file.display().to_string()), &Value::Null),
        "{}",
        r.context()
    );
    let written = fs::read_to_string(&file).unwrap();
    assert_eq!(written, first["data"]["content"].as_str().unwrap());
    // The same export again writes nothing.
    let same = home.run(&["export", "--write"]).expect(0);
    assert_eq!(same["data"]["path"], Value::Null);
    assert!(!bak.exists());

    // Something new: the previous file becomes the backup.
    release(&home, "Bar");
    home.run(&["install", "gh:o/bar", "-t", "retail"]).expect(0);
    let second = home.run(&["export", "--write"]).expect(0);
    assert_eq!(second["data"]["backup"], json!(bak.display().to_string()));
    assert_eq!(fs::read_to_string(&bak).unwrap(), written);

    // A hand edit is refused until --overwrite.
    let edited = format!("{}# mine\n", fs::read_to_string(&file).unwrap());
    fs::write(&file, &edited).unwrap();
    let r = home.run(&["export", "--write"]);
    let refused = r.expect(2);
    assert_eq!(
        (
            &refused["error"]["code"],
            &refused["error"]["details"]["reason"]
        ),
        (&json!("desired_state_invalid"), &json!("edited")),
        "{}",
        r.context()
    );
    let argv: Vec<String> = serde_json::from_value(refused["error"]["fix_argv"].clone()).unwrap();
    assert_eq!(argv, s(&["omaddons", "export", "--write", "--overwrite"]));
    assert_eq!(fs::read_to_string(&file).unwrap(), edited, "untouched");
    home.run(&["export", "--write", "--overwrite"]).expect(0);
    assert_eq!(
        fs::read_to_string(&bak).unwrap(),
        edited,
        "kept as the backup"
    );

    // Without the hash record, an existing file counts as edited.
    fs::remove_file(home.env.cache().join("omaddons/export.json")).unwrap();
    let r = home.run(&["export", "--write"]);
    assert_eq!(r.expect(2)["error"]["details"]["reason"], "edited");

    // A dry run checks and writes nothing.
    home.run(&["export", "--write", "--overwrite"]).expect(0);
    release(&home, "Baz");
    home.run(&["install", "gh:o/baz", "-t", "retail"]).expect(0);
    let before = fs::read_to_string(&file).unwrap();
    let dry = home.run(&["export", "--write", "--dry-run"]).expect(0);
    assert_eq!(dry["data"]["path"], Value::Null);
    assert_eq!(fs::read_to_string(&file).unwrap(), before);
}

#[test]
fn an_empty_export_is_never_written_and_overwrite_needs_write() {
    let home = Home::new(&["wow"]);
    let r = home.run(&["export", "--write"]);
    let envelope = r.expect(2);
    assert_eq!(
        (
            &envelope["error"]["code"],
            &envelope["error"]["details"]["reason"]
        ),
        (&json!("desired_state_invalid"), &json!("empty")),
        "{}",
        r.context()
    );
    assert!(!home.env.config().join("omaddons/addons.toml").exists());
    let envelope = home.run(&["export", "--overwrite"]).expect(2);
    assert_eq!(envelope["error"]["code"], "usage");
    assert_eq!(
        envelope["error"]["fix_argv"],
        json!(["omaddons", "export", "--overwrite", "--write"])
    );
}

#[test]
fn an_unplugged_client_is_exported_from_its_records() {
    let mut home = Home::new(&["wow"]);
    let dir = home.with_custom(
        CustomRoot::at(home.env.root().join("drive/wow-custom")).version(12, 1, 5, 64_000),
    );
    let targets = home.run(&["targets"]).expect(0);
    let id = targets["data"]["targets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["id"].as_str().unwrap().to_owned())
        .find(|id| id != "retail")
        .unwrap();
    let custom = "drive".to_owned();
    home.run(&["targets", "rename", &id, &custom]).expect(0);
    release(&home, "Foo");
    home.run(&["install", "gh:o/foo", "-t", &custom]).expect(0);
    // The drive is unplugged.
    fs::rename(&dir, home.env.root().join("unplugged")).unwrap();

    let r = home.run(&["export"]);
    let envelope = r.expect(0);
    let (_, state) = content(&envelope);
    assert_eq!(
        layout(&state),
        [(custom.clone(), s(&["Foo"]))],
        "{}",
        r.context()
    );
    let warning = envelope["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["code"] == "export_missing_target")
        .cloned();
    assert!(warning.is_some(), "{}", r.context());
    assert_eq!(warning.unwrap()["fix_argv"], json!(["omaddons", "targets"]));
    // -t names it by id.
    let envelope = home.run(&["export", "-t", &custom]).expect(0);
    assert_eq!(envelope["data"]["targets"], json!([custom]));
}
