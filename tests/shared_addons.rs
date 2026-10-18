//! Two clients sharing one AddOns directory (an `Interface/AddOns` symlinked between a
//! Classic Era and an Anniversary client): a folder one client's records own is
//! `shared` on the other, which may not record it too, not even with `--force`, so
//! one folder never has two owners. Records doubled before this was refused still
//! change, with a `target_addons_shared` warning.

mod common;

use std::fs;

use common::home::{Run, wait};
use serde_json::Value;
use support::{Shared, argv_of, assert_owned_on_era, codes};

#[cfg(test)]
mod support {
    use std::path::PathBuf;

    use super::*;
    use common::addons::toc;
    use common::home::Home;

    /// Era and Anniversary in one Battle.net root, Anniversary's AddOns a symlink to
    /// Era's, and an addon folder `Both` that loads on both.
    pub struct Shared {
        pub home: Home,
        pub both: PathBuf,
    }

    impl Shared {
        pub fn new() -> Self {
            let home = Home::new(&["wow_classic_era", "wow_anniversary"]);
            let both = home.env.root().join("src/Both");
            fs::create_dir_all(&both).unwrap();
            fs::write(
                both.join("Both.toc"),
                toc("11509, 20506", &["## Version: 1.0"]),
            )
            .unwrap();
            fs::write(both.join("b.lua"), "-- both").unwrap();
            let s = Self { home, both };
            s.link();
            s
        }

        /// Anniversary's AddOns becomes a symlink to Era's.
        pub fn link(&self) {
            let era = self.home.addons("_classic_era_");
            let anniversary = self.home.addons("_anniversary_");
            fs::create_dir_all(&era).unwrap();
            if anniversary.exists() {
                fs::remove_dir_all(&anniversary).unwrap();
            }
            std::os::unix::fs::symlink(&era, &anniversary).unwrap();
        }

        pub fn file_ref(&self) -> String {
            format!("file:{}", self.both.display())
        }

        pub fn run(&self, args: &[&str]) -> Run {
            self.home.run(args)
        }

        /// The addon names `list` shows on `target`.
        pub fn listed(&self, target: &str) -> Vec<String> {
            let envelope = self.run(&["list", "-t", target]).expect(0);
            envelope["data"]["targets"][0]["addons"]
                .as_array()
                .unwrap()
                .iter()
                .map(|a| a["name"].as_str().unwrap().to_owned())
                .collect()
        }
    }

    pub fn argv_of(value: &Value) -> Vec<String> {
        value
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a.as_str().unwrap().to_owned())
            .collect()
    }

    pub fn codes(envelope: &Value) -> Vec<String> {
        envelope["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|w| w["code"].as_str().unwrap().to_owned())
            .collect()
    }

    /// The refusal names the owner and its client, and its fix is the same command there.
    pub fn assert_owned_on_era(r: &Run) {
        let envelope = r.expect(4);
        let error = &envelope["error"];
        assert_eq!(error["code"], "ownership_conflict", "{}", r.context());
        let details = if error["details"]["folders"].is_array() {
            &error["details"]["folders"][0]
        } else {
            &error["details"]
        };
        assert_eq!(
            (
                &details["owner"],
                &details["owner_target"],
                &details["reason"]
            ),
            (
                &Value::from("Both"),
                &Value::from("era"),
                &Value::from("owned_on_shared_target")
            ),
            "{}",
            r.context()
        );
        let fix = argv_of(&error["fix_argv"]);
        let t = fix.iter().position(|a| a == "-t").unwrap();
        assert_eq!(fix[t + 1], "era", "{fix:?}");
    }
}

#[test]
fn a_folder_one_client_records_is_never_recorded_by_the_other() {
    let s = Shared::new();
    let both = s.both.display().to_string();
    s.run(&["install", &both, "-t", "era"]).expect(0);

    // Anniversary sees it as shared, not unmanaged, and nothing to adopt.
    let status = s.run(&["status", "-t", "anniversary"]).expect(0);
    let t = &status["data"]["targets"][0];
    assert_eq!(
        (
            &t["folders"]["shared"],
            &t["folders"]["unmanaged"],
            &t["adoptable"]
        ),
        (&Value::from(1), &Value::from(0), &Value::from(0)),
        "{status}"
    );
    let unmanaged = s
        .run(&["list", "-t", "anniversary", "--unmanaged"])
        .expect(0);
    let folder = &unmanaged["data"]["targets"][0]["unmanaged"][0];
    assert_eq!(
        (&folder["class"], &folder["owned_on"][0]["target"]),
        (&Value::from("shared"), &Value::from("era")),
        "{unmanaged}"
    );

    // Adopting or installing it there is refused, --force included.
    let file_ref = s.file_ref();
    assert_owned_on_era(&s.run(&[
        "adopt",
        "Both",
        "-t",
        "anniversary",
        "--ref",
        &file_ref,
        "--no-verify",
    ]));
    assert_owned_on_era(&s.run(&["install", &both, "-t", "anniversary"]));
    assert_owned_on_era(&s.run(&["install", &both, "-t", "anniversary", "--force"]));

    // The bulk adopt over every client records nothing more.
    s.run(&["adopt", "--apply", "--no-verify"]).expect(0);
    assert_eq!(s.listed("era"), ["Both"]);
    assert!(s.listed("anniversary").is_empty());

    // Era still changes it as before.
    let r = s.run(&["remove", "Both", "-t", "era"]);
    let envelope = r.expect(0);
    assert!(
        !codes(&envelope).contains(&"target_addons_shared".to_owned()),
        "{}",
        r.context()
    );
    assert!(!s.home.addons("_classic_era_").join("Both").exists());
}

/// Records made on both clients before sharing was checked stay doubled: a change
/// through one goes ahead and says the other sees it too.
#[test]
fn doubled_records_change_with_a_warning() {
    let s = Shared::new();
    // Record Both on each client while their AddOns are apart.
    let anniversary = s.home.addons("_anniversary_");
    fs::remove_file(&anniversary).unwrap();
    fs::create_dir_all(&anniversary).unwrap();
    let both = s.both.display().to_string();
    s.run(&["install", &both, "-t", "era"]).expect(0);
    s.run(&["install", &both, "-t", "anniversary"]).expect(0);
    fs::remove_dir_all(&anniversary).unwrap();
    s.link();
    assert_eq!(
        (s.listed("era"), s.listed("anniversary")),
        (vec!["Both".to_owned()], vec!["Both".to_owned()])
    );

    let r = s.run(&["remove", "Both", "-t", "era"]);
    let envelope = r.expect(0);
    let warning = envelope["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["code"] == "target_addons_shared");
    assert!(warning.is_some(), "{}", r.context());
    let warning = warning.unwrap();
    assert!(
        warning["message"].as_str().unwrap().contains("anniversary"),
        "{warning}"
    );
    assert_eq!(
        argv_of(&warning["fix_argv"]),
        ["omaddons", "status", "-t", "anniversary"]
    );
}

/// A client that records the folder while the other is planning, registering itself
/// on the way, is seen under the lock: the second install is refused even with
/// `--force`, and one record remains.
#[test]
fn a_sibling_that_records_it_meanwhile_is_seen_under_the_lock() {
    let s = Shared::new();
    let both = s.both.display().to_string();
    let args = ["install", both.as_str(), "-t", "anniversary", "--force"];
    let slow = s.home.spawn(
        &args,
        &[("OMADDONS_FAILPOINTS", "engine.plan.done=sleep:1500")],
    );
    std::thread::sleep(std::time::Duration::from_millis(500));
    s.run(&["install", &both, "-t", "era"]).expect(0);
    let r = wait(slow, &args);
    let envelope = r.envelope();
    assert_eq!(envelope["ok"], false, "{}", r.context());
    assert_eq!(
        envelope["error"]["code"],
        "ownership_conflict",
        "{}",
        r.context()
    );
    assert_eq!(s.listed("era"), ["Both"]);
    assert!(s.listed("anniversary").is_empty());
}
