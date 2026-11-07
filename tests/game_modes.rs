//! Game-mode names. Classic Era, Hardcore and Season of Discovery all run in one
//! client (`wow_classic_era`, `_classic_era_`) and share its AddOns folder, so `-t sod`
//! and `-t hardcore` select that client, `status` and `targets` list each client's modes,
//! and an install says when the folder is shared. Verified on a real machine on
//! 2026-10-15.

use crate::common;

use common::addons::toc;
use common::home::Home;
use common::http::{Asset, gh_release};
use common::trees::CustomRoot;
use serde_json::{Value, json};
use support::{foo_release, human, target};

#[cfg(test)]
mod support {
    use super::*;

    /// gh:o/foo loading on every live Classic client.
    pub fn foo_release(home: &Home) {
        let text = toc("11509, 20506, 50504", &["## Title: Foo"]);
        let assets: [Asset<'_>; 1] = [(
            "Foo.zip",
            &[("classic", 11_509), ("bcc", 20_506), ("mists", 50_504)],
            vec![("Foo/Foo.toc", text.as_str())],
        )];
        gh_release(&home.http, "https://github.com/o/foo", "v1", &assets);
    }

    /// The `id` entry of a status or targets envelope.
    pub fn target<'a>(envelope: &'a Value, id: &str) -> &'a Value {
        envelope["data"]["targets"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["id"] == id)
            .unwrap()
    }

    /// `omaddons <args>` in human form: (exit code, stdout).
    pub fn human(home: &Home, args: &[&str]) -> (i32, String) {
        let argv: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
        let r = home.exec(&argv);
        (r.code, r.stdout)
    }
}

const CLASSIC: &[&str] = &["wow_classic_era", "wow_classic", "wow_anniversary"];

#[test]
fn a_mode_selects_the_client_that_runs_it() {
    let home = Home::new(CLASSIC);
    foo_release(&home);
    for (selector, id, mode, shared) in [
        ("sod", "era", "sod", &["Classic Era", "Hardcore"][..]),
        ("SoD", "era", "sod", &["Classic Era", "Hardcore"]),
        (
            "season-of-discovery",
            "era",
            "sod",
            &["Classic Era", "Hardcore"],
        ),
        (
            "hardcore",
            "era",
            "hardcore",
            &["Classic Era", "Season of Discovery"],
        ),
        (
            "hc",
            "era",
            "hardcore",
            &["Classic Era", "Season of Discovery"],
        ),
        ("mop", "classic", "mists-classic", &[]),
        ("tbc-anniversary", "anniversary", "tbc-anniversary", &[]),
    ] {
        let r = home.run(&["install", "gh:o/foo", "-t", selector, "--dry-run"]);
        let envelope = r.expect(0);
        let t = &envelope["data"]["target"];
        assert_eq!(
            (&t["id"], &t["mode"]["id"], &t["mode"]["shared_with"]),
            (&Value::from(id), &Value::from(mode), &json!(shared)),
            "{selector}\n{}",
            r.context()
        );
    }
    // Naming the target itself is not a mode.
    let envelope = home
        .run(&["install", "gh:o/foo", "-t", "era", "--dry-run"])
        .expect(0);
    assert!(envelope["data"]["target"].get("mode").is_none());

    let (code, out) = human(&home, &["install", "gh:o/foo", "-t", "sod", "--dry-run"]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(
        out.lines().next(),
        Some(
            "era: Season of Discovery runs in this client and shares its addons with Classic Era and Hardcore"
        ),
        "{out}"
    );
}

#[test]
fn status_and_targets_list_each_clients_modes() {
    let home = Home::new(CLASSIC);
    let era_modes = json!([
        {"id": "classic-era", "name": "Classic Era"},
        {"id": "hardcore", "name": "Hardcore"},
        {"id": "sod", "name": "Season of Discovery"},
    ]);
    for command in ["status", "targets"] {
        let envelope = home.run(&[command]).expect(0);
        assert_eq!(target(&envelope, "era")["modes"], era_modes, "{command}");
        assert_eq!(
            target(&envelope, "classic")["modes"],
            json!([{"id": "mists-classic", "name": "Mists of Pandaria Classic"}]),
            "{command}"
        );
        assert_eq!(
            target(&envelope, "anniversary")["modes"],
            json!([{"id": "tbc-anniversary", "name": "Burning Crusade Anniversary"}]),
            "{command}"
        );

        // The human line names the modes of a client that runs several.
        let (code, out) = human(&home, &[command]);
        assert_eq!(code, 0, "{out}");
        let line = |id: &str| {
            out.lines()
                .find(|l| l.split_whitespace().any(|w| w == id))
                .unwrap_or_default()
                .to_owned()
        };
        assert!(
            line("era").contains("Classic Era · Hardcore · Season of Discovery"),
            "{command}: {out}"
        );
        assert!(line("classic").contains("mists"), "{command}: {out}");
    }
}

#[test]
fn a_mode_whose_client_is_missing_names_it() {
    let home = Home::new(&["wow"]);
    let r = home.run(&["status", "-t", "sod"]);
    let envelope = r.expect(3);
    let error = &envelope["error"];
    assert_eq!(
        (&error["code"], &error["message"], &error["fix_argv"]),
        (
            &Value::from("target_not_found"),
            &Value::from(
                "Season of Discovery runs in the Classic Era client (_classic_era_), which was not found on this machine"
            ),
            &json!(["omaddons", "targets", "--rescan"])
        ),
        "{}",
        r.context()
    );
}

#[test]
fn a_target_named_like_a_mode_keeps_its_name() {
    let home = Home::new(&["wow_classic_era"]);
    let custom = CustomRoot::at(home.env.home().join("Games/sod"))
        .version(1, 12, 1, 5875)
        .build();
    home.run(&[
        "targets",
        "add",
        &custom.display().to_string(),
        "--name",
        "sod",
    ])
    .expect(0);
    let envelope = home.run(&["status", "-t", "sod"]).expect(0);
    let targets = envelope["data"]["targets"].as_array().unwrap();
    assert_eq!(targets.len(), 1);
    assert_eq!(
        (&targets[0]["id"], &targets[0]["family"]),
        (&Value::from("sod"), &Value::from("vanilla-legacy"))
    );
    let envelope = home.run(&["status", "-t", "hardcore"]).expect(0);
    assert_eq!(envelope["data"]["targets"][0]["id"], "era");
}
