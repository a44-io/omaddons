use std::fs;
use std::path::PathBuf;

use jiff::{SignedDuration, Timestamp};
use serde_json::Value;

use super::super::Fixture;
use super::found_ids;
use crate::common::addons::toc;
use crate::common::archives::zip_text;
use crate::common::home::Home;

const ID: &str = "fm-state_files-snapshots-past-retention";

/// `fm-state_files-snapshots-past-retention`: retail keeps undo snapshots that retention
/// would prune now. Three real changes on retail (a local `Foo` installed, removed and
/// installed again), each leaving its snapshots, then their history lines aged to 10, 9
/// and 8 days, as a week of play without a change would leave them: retention reads a
/// transaction's age from its line, and nothing prunes a client nobody changes.
///
/// The finding: P3, tier `quick`, about retail (`fm-state_files-snapshots-past-retention@
/// <retail's key>`), title "retail keeps 2 undo snapshots retention would prune now",
/// evidence `{path: <retail>/Interface/.omaddons/snapshots, count: 2, txs: [the two
/// oldest], oldest_at}`: the newest is the client's most recent transaction, which
/// retention always keeps. Its remediation is advice (retention prunes them with the next
/// change), and the remedy is that change: one more install on retail, whose own
/// retention prunes both. The subject is the two transactions' snapshot halves.
#[derive(Default)]
pub struct SnapshotsPastRetention {
    txs: std::cell::RefCell<Vec<String>>,
}

impl SnapshotsPastRetention {
    /// A local zip of one folder `name`, loading on retail.
    fn zip(home: &Home, name: &str) -> String {
        let dl = home.env.root().join("dl");
        fs::create_dir_all(&dl).unwrap();
        let zip = dl.join(format!("{name}.zip"));
        let text = toc("120105", &["## Version: 1"]);
        let (toc_path, lua) = (format!("{name}/{name}.toc"), format!("{name}/core.lua"));
        fs::write(&zip, zip_text(&[(&toc_path, &text), (&lua, "1")])).unwrap();
        zip.display().to_string()
    }

    fn run(home: &Home, args: &[&str]) -> Value {
        let mut argv: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
        argv.push("--json".to_owned());
        let run = home.exec(&argv);
        assert_eq!(run.code, 0, "{args:?}: {}", run.stdout);
        serde_json::from_str(&run.stdout).unwrap()
    }
}

impl Fixture for SnapshotsPastRetention {
    fn id(&self) -> &'static str {
        ID
    }

    fn corrupt(&self, home: &Home) {
        let foo = Self::zip(home, "Foo");
        let mut txs = Vec::new();
        for args in [
            &["install", foo.as_str(), "-t", "retail"][..],
            &["remove", "Foo", "-t", "retail"],
            &["install", foo.as_str(), "-t", "retail"],
        ] {
            let envelope = Self::run(home, args);
            txs.push(envelope["data"]["tx"].as_str().unwrap().to_owned());
        }
        // Oldest first: 10, 9 and 8 days old.
        let now = Timestamp::now();
        let path = home.state().join("history.jsonl");
        let mut out = String::new();
        for line in fs::read_to_string(&path).unwrap().lines() {
            let mut value: Value = serde_json::from_str(line).unwrap();
            if let Some(at) = txs.iter().position(|tx| value["tx"] == tx.as_str()) {
                let days = 10 - i64::try_from(at).unwrap();
                value["at"] = (now - SignedDuration::from_hours(24 * days))
                    .to_string()
                    .into();
            }
            out.push_str(&serde_json::to_string(&value).unwrap());
            out.push('\n');
        }
        fs::write(&path, out).unwrap();
        self.txs.replace(txs);
    }

    /// No instance is found, and the two oldest transactions' snapshots are gone.
    fn assert_healthy(&self, home: &Home) {
        let ids = found_ids(home, &["--only".to_owned(), ID.to_owned()]);
        assert!(!ids.iter().any(|id| id == ID), "{ID} still found: {ids:?}");
        for path in self.subject(home) {
            assert!(!path.exists(), "{} still there", path.display());
        }
    }

    /// The client and state halves of the two oldest transactions.
    fn subject(&self, home: &Home) -> Vec<PathBuf> {
        let client = home
            .client("_retail_")
            .join("Interface/.omaddons/snapshots");
        let state = home.state().join("snapshots");
        self.txs
            .borrow()
            .iter()
            .take(2)
            .flat_map(|tx| [client.join(tx), state.join(tx)])
            .collect()
    }

    /// The next change on retail: its own retention prunes them.
    fn remedy(&self, home: &Home) {
        let bar = Self::zip(home, "Bar");
        Self::run(home, &["install", bar.as_str(), "-t", "retail"]);
    }
}
