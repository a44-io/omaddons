//! The TOC load-rule matrix (vision §7, M1 checkpoint 3): real folders on disk judged by
//! `loaded_toc` and `verdict` for every builtin client-rules row.
//!
//! Group 1, the suffix sweep, is generated here: per row, a folder with every known
//! suffix (both separators) and a plain TOC, peeled level by level. Groups 2–10 are data
//! in `tests/fixtures/toc/matrix/cases.toml`, so a new flavor is new rows. The literal
//! guard keeps client versions out of the TOC and inventory code.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use common::env::TestEnv;
use common::targets::battlenet;
use omaddons::rules::{
    ClientRules, ClientVersion, Family, InstallKind, KNOWN_TOC_SUFFIXES, RulesTable,
};
use omaddons::toc::{DiskFolder, loaded_toc, verdict};
use serde::Deserialize;
use support::{Client, SWEEP, cases, folder, summary};

#[cfg(test)]
mod support {
    use super::*;

    /// Every builtin row with a client version it covers (group 1).
    pub const SWEEP: &[(&str, &str, InstallKind)] = &[
        ("mainline", "12.1.5", InstallKind::Battlenet),
        ("mainline-12.0.7", "12.1.0", InstallKind::Battlenet),
        ("mainline-10.2.7", "12.0.1", InstallKind::Battlenet),
        ("mainline-9.1", "10.2.6", InstallKind::Battlenet),
        ("forever", "1.60.1", InstallKind::Battlenet),
        ("vanilla", "1.15.9", InstallKind::Battlenet),
        ("tbc", "2.5.6", InstallKind::Battlenet),
        ("wrath", "3.4.3", InstallKind::Battlenet),
        // Titan loads `_Wrath` TOCs (from publishers' archives, 2026-10-15).
        ("titan", "3.80.2", InstallKind::Battlenet),
        ("cata", "4.4.2", InstallKind::Battlenet),
        ("mists", "5.5.4", InstallKind::Battlenet),
        ("vanilla-legacy", "1.12.1", InstallKind::Custom),
        ("tbc-legacy", "2.4.3", InstallKind::Custom),
        ("wrath-legacy", "3.3.5", InstallKind::Custom),
        ("cata-legacy", "4.3.4", InstallKind::Custom),
        ("mists-legacy", "5.4.8", InstallKind::Custom),
        ("fallback-battlenet", "2.6.0", InstallKind::Battlenet),
        ("fallback-custom", "2.0.0", InstallKind::Custom),
    ];

    /// A client as the verdict sees it.
    pub struct Client {
        pub rules: ClientRules,
        pub family: Family,
        pub interface: u32,
    }

    impl Client {
        /// Family, row and interface straight from the tables.
        pub fn from_version(table: &RulesTable, version: &str, kind: InstallKind) -> Self {
            let version: ClientVersion = version.parse().unwrap();
            let rules = table.rules_for(&version, kind).clone();
            Self {
                family: table.family_for(&version, kind),
                interface: table.interface_for_client(&version, kind, &rules),
                rules,
            }
        }

        /// A Battle.net product's client, built from a synthetic tree by discovery.
        pub fn from_product(env: &TestEnv, code: &str) -> Self {
            let target = battlenet(env, code);
            Self {
                rules: target.rules,
                family: target.family,
                interface: target.interface,
            }
        }
    }

    #[derive(Debug, Deserialize)]
    struct Cases {
        case: Vec<Case>,
    }

    #[derive(Debug, Deserialize)]
    #[serde(untagged)]
    pub enum TargetSpec {
        Product { product: String },
        Version { version: String, kind: InstallKind },
    }

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct Case {
        pub group: String,
        pub name: String,
        pub target: TargetSpec,
        pub files: Vec<(String, String)>,
        #[serde(default)]
        pub expect_file: Option<String>,
        pub expect_verdict: String,
        #[serde(default)]
        pub expect_reason: Option<String>,
        #[serde(default)]
        pub expect_out_of_date: bool,
        #[serde(default)]
        pub expect_warnings: Vec<String>,
        #[serde(default)]
        pub expect_case_ambiguous: bool,
    }

    impl Case {
        pub fn client(&self, env: &TestEnv, table: &RulesTable) -> Client {
            match &self.target {
                TargetSpec::Product { product } => Client::from_product(env, product),
                TargetSpec::Version { version, kind } => {
                    Client::from_version(table, version, *kind)
                }
            }
        }
    }

    pub fn cases() -> Vec<Case> {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/toc/matrix/cases.toml");
        let text = fs::read_to_string(path).unwrap();
        toml::from_str::<Cases>(&text).unwrap().case
    }

    /// `<dir>/Foo` with `files`; returns the folder.
    pub fn folder(dir: &Path, files: &[(String, String)]) -> PathBuf {
        let path = dir.join("Foo");
        fs::create_dir_all(&path).unwrap();
        for (name, text) in files {
            fs::write(path.join(name), format!("{text}\n")).unwrap();
        }
        path
    }

    /// One summary line per group on stderr.
    pub fn summary(group: &str, count: usize) {
        let _ = writeln!(std::io::stderr(), "toc matrix: {group}: {count} cases");
    }
}

#[test]
fn suffix_sweep() {
    let env = TestEnv::new();
    let table = RulesTable::builtin();
    for (i, (row, version, kind)) in SWEEP.iter().enumerate() {
        let client = Client::from_version(&table, version, *kind);
        assert_eq!(
            client.rules.name, *row,
            "{version} {kind:?} maps to the {row} row"
        );
        let rules = &client.rules;
        let mut suffixes: Vec<String> =
            KNOWN_TOC_SUFFIXES.iter().map(|s| (*s).to_owned()).collect();
        for s in rules
            .game_suffixes
            .iter()
            .chain(&rules.family_suffixes)
            .chain(&rules.legacy_suffixes)
        {
            if !suffixes.iter().any(|k| k.eq_ignore_ascii_case(s)) {
                suffixes.push(s.clone());
            }
        }
        let interface = client.interface;
        let mut files = vec![(
            "Foo.toc".to_owned(),
            format!("## Interface: {interface}\n## Title: plain"),
        )];
        for s in &suffixes {
            for sep in ['_', '-'] {
                let file = format!("Foo{sep}{s}.toc");
                files.push((
                    file.clone(),
                    format!("## Interface: {interface}\n## Title: {file}"),
                ));
            }
        }
        let dir = folder(&env.root().join(format!("sweep/{i}")), &files);
        let loaded = || loaded_toc(&DiskFolder::new(&dir).unwrap(), rules).unwrap();

        // Precedence: game suffixes in order, then legacy, then family, then plain.
        let mut order: Vec<Option<&String>> = Vec::new();
        if rules.multi_toc {
            order.extend(rules.game_suffixes.iter().map(Some));
            order.extend(rules.legacy_suffixes.iter().map(Some));
            order.extend(rules.family_suffixes.iter().map(Some));
        }
        order.push(None);
        for expected in order {
            match expected {
                Some(s) => {
                    // Both separators load; between them the byte order decides.
                    for sep in ['-', '_'] {
                        let file = format!("Foo{sep}{s}.toc");
                        let got = loaded();
                        assert_eq!(
                            got.as_ref().map(|l| l.file.as_str()),
                            Some(file.as_str()),
                            "{row}: expected {file}; left: {:?}",
                            fs::read_dir(&dir).unwrap().count()
                        );
                        assert!(
                            got.as_ref()
                                .and_then(|l| l.suffix.as_deref())
                                .is_some_and(|x| x.eq_ignore_ascii_case(s)),
                            "{row}: {got:?}"
                        );
                        fs::remove_file(dir.join(&file)).unwrap();
                    }
                }
                None => {
                    let got = loaded();
                    assert_eq!(
                        got.as_ref().map(|l| l.file.as_str()),
                        Some("Foo.toc"),
                        "{row}: plain"
                    );
                    fs::remove_file(dir.join("Foo.toc")).unwrap();
                }
            }
        }
        // Only suffixes this row does not load are left.
        assert!(loaded().is_none(), "{row}: {:?}", loaded());
    }
    summary("suffix_sweep", SWEEP.len());
}

#[test]
fn curated_cases() {
    let env = TestEnv::new();
    let table = RulesTable::builtin();
    let mut groups: BTreeMap<String, usize> = BTreeMap::new();
    let mut products: BTreeMap<String, Client> = BTreeMap::new();
    for (i, case) in cases().iter().enumerate() {
        let owned;
        let client: &Client = match &case.target {
            support::TargetSpec::Product { product } => products
                .entry(product.clone())
                .or_insert_with(|| Client::from_product(&env, product)),
            support::TargetSpec::Version { .. } => {
                owned = case.client(&env, &table);
                &owned
            }
        };
        let dir = folder(&env.root().join(format!("cases/{i}")), &case.files);
        let view = DiskFolder::new(&dir).unwrap();
        let report = verdict(
            &view,
            &client.rules,
            client.family,
            client.interface,
            &table,
        )
        .unwrap();
        let value = serde_json::to_value(&report).unwrap();
        let context = format!(
            "case {} ({}): files {:?}, row {}, family {:?}, interface {}\nreport: {}",
            case.name,
            case.group,
            case.files.iter().map(|(f, _)| f).collect::<Vec<_>>(),
            client.rules.name,
            client.family,
            client.interface,
            serde_json::to_string_pretty(&value).unwrap()
        );
        assert_eq!(
            value["loaded"]["file"].as_str(),
            case.expect_file.as_deref(),
            "{context}"
        );
        assert_eq!(
            value["verdict"]["verdict"],
            case.expect_verdict.as_str(),
            "{context}"
        );
        if let Some(reason) = &case.expect_reason {
            assert_eq!(value["verdict"]["reason"], reason.as_str(), "{context}");
        }
        if case.expect_verdict.eq("compatible") {
            assert_eq!(
                value["verdict"]["out_of_date"], case.expect_out_of_date,
                "{context}"
            );
        }
        let mut warnings: Vec<&str> = value["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|w| w["code"].as_str().unwrap())
            .collect();
        warnings.sort_unstable();
        let mut expected: Vec<&str> = case.expect_warnings.iter().map(String::as_str).collect();
        expected.sort_unstable();
        assert_eq!(warnings, expected, "{context}");
        if !value["loaded"].is_null() {
            assert_eq!(
                value["loaded"]["case_ambiguous"], case.expect_case_ambiguous,
                "{context}"
            );
        }
        *groups.entry(case.group.clone()).or_default() += 1;
    }
    for (group, count) in &groups {
        summary(group, *count);
    }
}

#[test]
fn every_group_and_every_builtin_row_are_covered() {
    let table = RulesTable::builtin();
    let env = TestEnv::new();
    let mut groups: BTreeSet<String> = cases().iter().map(|c| c.group.clone()).collect();
    groups.insert("suffix_sweep".to_owned());
    let expected: BTreeSet<String> = [
        "suffix_sweep",
        "forever_guard",
        "standard_boundary",
        "comma_lists",
        "legacy_rows",
        "cross_family",
        "case_naming",
        "out_of_date",
        "game_types",
        "unknown_family",
        "inline_directives",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    assert_eq!(groups, expected);

    let mut covered: BTreeSet<String> = SWEEP.iter().map(|(row, ..)| (*row).to_owned()).collect();
    for case in cases() {
        covered.insert(case.client(&env, &table).rules.name.clone());
    }
    let mut rows: Vec<String> = table
        .client_rules()
        .iter()
        .map(|r| r.name.clone())
        .collect();
    rows.push(table.fallback(InstallKind::Battlenet).name.clone());
    rows.push(table.fallback(InstallKind::Custom).name.clone());
    for row in rows {
        assert!(covered.contains(&row), "the {row} row has no case");
    }

    let named: Vec<String> = cases()
        .iter()
        .filter(|c| {
            c.name
                .eq("modern-classic-flavor-build-never-verifies-against-1.12.1")
        })
        .map(|c| c.expect_verdict.clone())
        .collect();
    assert_eq!(named, ["wrong_flavor"], "the named checkpoint case exists");
}

#[test]
fn no_client_literals_outside_rules() {
    const LITERALS: &[&str] = &[
        "120105", "16001", "38002", "30300", "11200", "20400", "40300", "50400", "11509", "20506",
        "50504", "40402",
    ];
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files: Vec<PathBuf> = vec![root.join("src/toc.rs"), root.join("src/inventory.rs")];
    if let Ok(dir) = fs::read_dir(root.join("src/toc")) {
        files.extend(dir.map(|e| e.unwrap().path()));
    }
    for file in files.into_iter().filter(|f| f.exists()) {
        let text = fs::read_to_string(&file).unwrap();
        let code = text.split("#[cfg(test)]").next().unwrap_or_default();
        for (n, line) in code.lines().enumerate() {
            let line = line.split("//").next().unwrap_or_default();
            for literal in LITERALS {
                for (at, _) in line.match_indices(literal) {
                    let before = line[..at].chars().last();
                    let after = line[at + literal.len()..].chars().next();
                    let digit = |c: Option<char>| c.is_some_and(|c| c.is_ascii_digit() || c == '_');
                    assert!(
                        digit(before) || digit(after),
                        "{}:{}: client literal {literal}: {line}",
                        file.display(),
                        n + 1
                    );
                }
            }
            // A quoted version such as "12.1.5".
            for (at, _) in line.match_indices('"') {
                let rest = &line[at + 1..];
                let parts: Vec<&str> = rest
                    .split(['"', ' '])
                    .next()
                    .unwrap_or_default()
                    .split('.')
                    .collect();
                let version = parts.len() >= 3
                    && parts
                        .iter()
                        .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()));
                assert!(
                    !version,
                    "{}:{}: quoted client version: {line}",
                    file.display(),
                    n + 1
                );
            }
        }
    }
}
