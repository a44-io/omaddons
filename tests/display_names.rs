//! e2e suite: display_name, the name a player knows each client by, in status, status --cached and targets. Every multi-command scenario is a TestLog scenario.
//!
//! A home laid out like the development machine (five Battle.net clients and a 3.3.5a
//! private server) names its clients as players do; a name set with `config set
//! targets.<id>.name` wins and an empty one restores the rule; names stay unique in one
//! envelope.

use crate::common;

use std::collections::BTreeMap;

use common::home::Home;
use common::tlog::{TestLog, tlog};
use common::trees::CustomRoot;
use serde_json::Value;

/// `display_name` by id, from a `status` or `targets` envelope's `data.targets`.
/// A target without either key shows as an empty string, which the comparisons catch.
fn names(data: &Value) -> BTreeMap<String, String> {
    data["targets"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|t| {
            let text = |key: &str| t[key].as_str().unwrap_or_default().to_owned();
            (text("id"), text("display_name"))
        })
        .collect()
}

/// Every command that lists clients, each as `(argv, display names by id)`.
fn every_listing(log: &TestLog, home: &Home) -> Vec<(String, BTreeMap<String, String>)> {
    [
        &["status", "--json"][..],
        &["status", "--cached", "--json"],
        &["targets", "--json"],
    ]
    .iter()
    .map(|argv| {
        let ran = log.run(home, argv);
        ran.ok();
        (argv.join(" "), names(ran.data()))
    })
    .collect()
}

#[test]
fn every_listing_names_the_clients_as_players_do() {
    let log = tlog!();
    log.step("a home like the development machine");
    let mut home = Home::new(&[
        "wow",
        "wow_classic",
        "wow_classic_era",
        "wow_anniversary",
        "wowf",
    ]);
    let root = home.env.root().join("pserver");
    home.with_custom(CustomRoot::at(root));

    log.step("the rule");
    let expected: BTreeMap<String, String> = [
        ("anniversary", "Burning Crusade Anniversary"),
        ("classic", "Mists of Pandaria Classic"),
        ("era", "Classic Era"),
        ("forever", "WoW: Forever"),
        ("retail", "Retail"),
        ("wotlk", "Wrath 3.3.5a private server"),
    ]
    .iter()
    .map(|(id, name)| ((*id).to_owned(), (*name).to_owned()))
    .collect();
    for (argv, got) in every_listing(&log, &home) {
        log.assert_eq(&argv, &got, &expected);
    }

    log.step("a chosen name wins, in every listing");
    log.run(&home, &["config", "set", "targets.retail.name", "My main"])
        .ok();
    let got = log.run(&home, &["config", "get", "targets.retail.name", "--json"]);
    log.assert_eq("config get", &got.data()["value"], &Value::from("My main"));
    for (argv, got) in every_listing(&log, &home) {
        log.assert_eq(&argv, &got["retail"], &"My main".to_owned());
    }

    log.step("a name two clients would share is told apart by the id");
    log.run(&home, &["config", "set", "targets.era.name", "My main"])
        .ok();
    for (argv, got) in every_listing(&log, &home) {
        log.assert_eq(
            &argv,
            &(got["era"].clone(), got["retail"].clone()),
            &("My main (era)".to_owned(), "My main (retail)".to_owned()),
        );
    }

    log.step("an empty name restores the rule");
    log.run(&home, &["config", "set", "targets.retail.name", ""])
        .ok();
    log.run(&home, &["config", "set", "targets.era.name", ""])
        .ok();
    for (argv, got) in every_listing(&log, &home) {
        log.assert_eq(&argv, &got, &expected);
    }
}
