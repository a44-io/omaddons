//! `-t` across several clients (M2-D24): read commands fan a family or mode selector out
//! to every matching client, while `install` still refuses to guess one.

mod common;

use common::home::Home;
use common::trees::CustomRoot;
use serde_json::Value;
use support::{ids, two_vanilla};

#[cfg(test)]
mod support {
    use super::*;

    /// A Battle.net Classic Era client and a 1.15.x custom client: two vanilla clients.
    pub fn two_vanilla() -> Home {
        let home = Home::new(&["wow", "wow_classic_era"]);
        CustomRoot::at(home.env.home().join("Games/era-custom"))
            .version(1, 15, 9, 60_003)
            .build();
        // Its default id, `vanilla`, would win over the family name: rename it.
        home.run(&["targets", "rename", "vanilla", "custom"])
            .expect(0);
        let targets = home.run(&["targets"]).expect(0);
        let vanilla: Vec<&str> = targets["data"]["targets"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|t| t["family"] == "vanilla")
            .map(|t| t["id"].as_str().unwrap())
            .collect();
        assert_eq!(vanilla.len(), 2, "{targets}");
        home
    }

    /// The `id`s of `values`, where `field` names the id within each.
    pub fn ids(values: &Value, field: &str) -> Vec<String> {
        values
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v[field].as_str().unwrap().to_owned())
            .collect()
    }
}

#[test]
fn read_commands_cover_every_client_a_family_names() {
    let home = two_vanilla();
    let r = home.run(&["status", "-t", "vanilla"]);
    let status = r.expect(0);
    let covered = ids(&status["data"]["targets"], "id");
    assert_eq!(covered.len(), 2, "{}", r.context());
    assert!(covered.contains(&"era".to_owned()), "{covered:?}");
    let mut sorted = covered.clone();
    sorted.sort();
    assert_eq!(covered, sorted, "sorted by id");

    let r = home.run(&["list", "-t", "vanilla"]);
    let list = r.expect(0);
    assert_eq!(
        ids(&list["data"]["targets"], "id"),
        covered,
        "{}",
        r.context()
    );
    let r = home.run(&["list", "-t", "mainline"]);
    assert_eq!(
        ids(&r.expect(0)["data"]["targets"], "id"),
        ["retail"],
        "{}",
        r.context()
    );

    // A single target reads exactly as before.
    let r = home.run(&["status", "-t", "retail"]);
    assert_eq!(ids(&r.expect(0)["data"]["targets"], "id"), ["retail"]);
    // A mode selects the client running it.
    let r = home.run(&["status", "-t", "sod"]);
    assert_eq!(
        ids(&r.expect(0)["data"]["targets"], "id"),
        ["era"],
        "{}",
        r.context()
    );
    // Nothing matching: the same error as before.
    let r = home.run(&["status", "-t", "nosuch"]);
    assert_eq!(r.expect(3)["error"]["code"], "target_not_found");
}

#[test]
fn info_covers_every_client_a_family_names() {
    let home = two_vanilla();
    let dl = home.env.root().join("dl");
    std::fs::create_dir_all(&dl).unwrap();
    let zip = dl.join("Foo.zip");
    let toc = "## Interface: 11509\n## Version: 1\n";
    std::fs::write(&zip, common::archives::zip_text(&[("Foo/Foo.toc", toc)])).unwrap();
    let path = zip.display().to_string();
    let status = home.run(&["status", "-t", "vanilla"]).expect(0);
    for id in ids(&status["data"]["targets"], "id") {
        let r = home.run(&["install", "gh:o/foo", "--file", &path, "-t", &id]);
        r.expect(0);
    }
    let r = home.run(&["info", "gh:o/foo", "-t", "vanilla", "--offline"]);
    let envelope = r.expect(0);
    let installed: Vec<String> = envelope["data"]["installed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["target"]["id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(installed.len(), 2, "{}", r.context());
}

#[test]
fn install_still_refuses_to_guess_among_a_familys_clients() {
    let home = two_vanilla();
    let dl = home.env.root().join("dl");
    std::fs::create_dir_all(&dl).unwrap();
    let zip = dl.join("Foo.zip");
    let toc = "## Interface: 11509\n";
    std::fs::write(&zip, common::archives::zip_text(&[("Foo/Foo.toc", toc)])).unwrap();
    let path = zip.display().to_string();
    let r = home.run(&["install", "gh:o/foo", "--file", &path, "-t", "vanilla"]);
    let envelope = r.expect(3);
    let error = &envelope["error"];
    assert_eq!(error["code"], "target_ambiguous", "{}", r.context());
    let mut fixes = vec![error["fix_argv"].clone()];
    fixes.extend(
        error["alternatives"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["argv"].clone()),
    );
    assert_eq!(fixes.len(), 2, "one fix per candidate\n{}", r.context());
    for fix in fixes {
        let argv: Vec<String> = fix
            .as_array()
            .unwrap()
            .iter()
            .skip(1)
            .map(|a| a.as_str().unwrap().to_owned())
            .chain(["--json".to_owned()])
            .collect();
        let run = home.exec(&argv);
        assert_eq!(run.code, 0, "{}", run.context());
    }
}
