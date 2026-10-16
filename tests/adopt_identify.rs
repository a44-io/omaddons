//! Adoption's grouping end to end over real folders (M2-D40): fixture TOC sets
//! (`tests/fixtures/adopt/sets.toml`) are written into a synthetic retail client, read
//! back through the inventory's loaded-TOC summaries, and grouped by `identify` with the
//! builtin catalog or fakes for the catalog, the Tukui list and the WoWInterface index.
//! Every case also checks the invariants: each candidate in exactly one group, nothing
//! `exact`, no unloaded folders, alternatives capped and never the group's own ref, only
//! installable refs, every ref named by some evidence, and the same result in any order.

mod common;

use common::env::TestEnv;
use omaddons::model::Ref;
use omaddons::resolve::adopt::{Evidence, Group, Strength, Tier};
use omaddons::rules::Family;
use support::{Cat, Client, Wowi, entry, groups, project, set};

#[cfg(test)]
mod support {
    use std::collections::BTreeMap;
    use std::path::Path;

    use omaddons::discover::DiscoverySource;
    use omaddons::discover::battlenet::{BattlenetHints, discover_root};
    use omaddons::inventory::{Depth, inventory};
    use omaddons::limits::ADOPT_MAX_ALTERNATIVES;
    use omaddons::model::{FolderName, Ref, Target};
    use omaddons::resolve::adopt::{
        AdoptCatalog, AdoptLookups, AdoptTukui, AdoptWowi, CandidateFolder, Group, Tier,
        TukuiProject, WowiProject, evidence, identify, normalise,
    };
    use omaddons::resolve::catalog::{Catalog, Entry};
    use omaddons::rules::{Family, RulesTable};
    use omaddons::sources::wowi::index::sanitize_dirs;

    use super::common::env::TestEnv;
    use super::common::trees::BattlenetRoot;

    /// A fixture set's folders and TOCs.
    pub fn set(name: &str) -> Vec<(String, String)> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/adopt/sets.toml");
        let sets: toml::Table = std::fs::read_to_string(path).unwrap().parse().unwrap();
        sets[name]
            .as_table()
            .unwrap()
            .iter()
            .map(|(folder, v)| (folder.clone(), v["toc"].as_str().unwrap().to_owned()))
            .collect()
    }

    /// A retail client whose AddOns holds the folders written into it.
    pub struct Client {
        pub target: Target,
    }

    impl Client {
        pub fn new(env: &TestEnv) -> Self {
            let root = BattlenetRoot::at(env.root().join("wow"))
                .product("wow")
                .build();
            let (targets, _) = discover_root(
                &root,
                &RulesTable::builtin(),
                &BattlenetHints::default(),
                "battlenet",
                &DiscoverySource::Scan,
            );
            let target = targets
                .into_iter()
                .find(|t| t.target.flavor_dir.as_deref() == Some("_retail_"))
                .unwrap()
                .target;
            Self { target }
        }

        /// Writes `<folder>/<folder>.toc` for each.
        pub fn with(self, folders: &[(String, String)]) -> Self {
            for (folder, toc) in folders {
                let dir = self.target.client_dir.join("Interface/AddOns").join(folder);
                std::fs::create_dir_all(&dir).unwrap();
                std::fs::write(dir.join(format!("{folder}.toc")), toc).unwrap();
            }
            self
        }

        /// The candidates, as the inventory reads them.
        pub fn candidates(&self) -> Vec<CandidateFolder> {
            inventory(&self.target, None, &RulesTable::builtin(), Depth::Full)
                .unwrap()
                .folders
                .iter()
                .filter_map(CandidateFolder::from_entry)
                .collect()
        }
    }

    /// The builtin catalog (or none) plus extra entries.
    pub struct Cat {
        pub builtin: Option<Catalog>,
        pub extra: Vec<Entry>,
    }

    impl Cat {
        pub fn builtin() -> Self {
            Self {
                builtin: Some(Catalog::builtin().unwrap()),
                extra: Vec::new(),
            }
        }

        pub fn only(extra: Vec<Entry>) -> Self {
            Self {
                builtin: None,
                extra,
            }
        }
    }

    impl AdoptCatalog for Cat {
        fn by_folder(&self, folder: &FolderName) -> Vec<&Entry> {
            let mut found: Vec<&Entry> = self
                .builtin
                .as_ref()
                .map(|c| c.by_folder(folder))
                .unwrap_or_default();
            found.extend(self.extra.iter().filter(|e| e.folders.contains(folder)));
            found
        }

        fn by_name(&self, name: &str) -> Option<&Entry> {
            self.builtin
                .as_ref()
                .and_then(|c| c.by_name(name))
                .or_else(|| self.extra.iter().find(|e| e.answers_to(name)))
        }
    }

    pub fn entry(
        name: &str,
        aliases: &[&str],
        r#ref: &str,
        families: &[Family],
        folders: &[&str],
    ) -> Entry {
        Entry {
            name: name.into(),
            aliases: aliases.iter().map(|a| (*a).to_owned()).collect(),
            r#ref: r#ref.parse().unwrap(),
            families: families.to_vec(),
            folders: folders
                .iter()
                .map(|f| FolderName::new(f).unwrap())
                .collect(),
            notes: None,
        }
    }

    /// A fake WoWInterface index: projects with `dirs` sanitised as the index does.
    #[derive(Default)]
    pub struct Wowi(pub BTreeMap<u64, WowiProject>);

    pub fn project(
        uid: u64,
        name: &str,
        author: &str,
        dirs: &[&str],
        downloads: u64,
    ) -> (u64, WowiProject) {
        let raw: Vec<String> = dirs.iter().map(|d| (*d).to_owned()).collect();
        (
            uid,
            WowiProject {
                uid,
                name: name.into(),
                author: author.into(),
                dirs: sanitize_dirs(&raw),
                downloads,
                date_ms: 1_600_000_000_000,
            },
        )
    }

    impl AdoptWowi for Wowi {
        fn by_folder(&self, folder: &FolderName) -> Vec<u64> {
            self.0
                .values()
                .filter(|p| p.dirs.contains(folder))
                .map(|p| p.uid)
                .collect()
        }

        fn by_uid(&self, uid: u64) -> Option<WowiProject> {
            self.0.get(&uid).cloned()
        }

        fn by_name(&self, normalised: &str) -> Vec<u64> {
            self.0
                .values()
                .filter(|p| normalise(&p.name) == normalised)
                .map(|p| p.uid)
                .collect()
        }
    }

    pub struct NoTukui;

    impl AdoptTukui for NoTukui {
        fn by_id(&self, _: i64) -> Option<TukuiProject> {
            None
        }

        fn by_slug(&self, _: &str) -> Option<TukuiProject> {
            None
        }
    }

    /// `identify` on retail, checked against every invariant (including order
    /// independence).
    pub fn groups(
        candidates: &[CandidateFolder],
        catalog: &Cat,
        wowi: Option<&Wowi>,
    ) -> Vec<Group> {
        let lookups = AdoptLookups {
            family: Family::Mainline,
            catalog,
            tukui: Some(&NoTukui),
            wowi: wowi.map(|w| w as &dyn AdoptWowi),
        };
        let groups = identify(candidates, &lookups);
        check(candidates, &lookups, &groups);
        let mut reversed = candidates.to_vec();
        reversed.reverse();
        assert_eq!(identify(&reversed, &lookups), groups, "order independent");
        if candidates.len() > 2 {
            let mut rotated = candidates.to_vec();
            rotated.rotate_left(1);
            assert_eq!(identify(&rotated, &lookups), groups, "order independent");
        }
        groups
    }

    /// The invariants every result keeps.
    pub fn check(candidates: &[CandidateFolder], lookups: &AdoptLookups<'_>, groups: &[Group]) {
        for c in candidates {
            let holding = groups
                .iter()
                .filter(|g| g.folders.contains(&c.folder))
                .count();
            assert_eq!(
                holding,
                1,
                "{} in {holding} groups: {groups:#?}",
                c.folder.as_str()
            );
        }
        let total: usize = groups.iter().map(|g| g.folders.len()).sum();
        assert_eq!(total, candidates.len(), "no folder twice, nothing invented");
        for g in groups {
            assert!(
                g.unloaded_folders.is_empty() && g.tier != Tier::Exact,
                "{g:#?}"
            );
            assert!(g.alternatives.len() <= ADOPT_MAX_ALTERNATIVES, "{g:#?}");
            assert!(g.folders.contains(&g.primary));
            let own = g.claim.as_ref().and_then(|c| c.r#ref.clone());
            assert!(
                own.as_ref().is_none_or(|r| !g.alternatives.contains(r)),
                "{g:#?}"
            );
            for r in own.iter().chain(&g.alternatives) {
                assert!(
                    matches!(r, Ref::GitHub { .. } | Ref::Wowi { .. } | Ref::Tukui { .. }),
                    "{r}"
                );
            }
            // The group's ref is one some evidence names for one of its folders.
            if let Some(r) = &own {
                let named = candidates.iter().any(|c| {
                    evidence(c, lookups).iter().any(|claim| {
                        claim.r#ref.as_ref() == Some(r)
                            && claim.folders.iter().any(|f| g.folders.contains(f))
                    })
                });
                assert!(named, "{r} is named by no evidence: {g:#?}");
            }
            match g.tier {
                Tier::Unknown => assert!(g.claim.is_none()),
                Tier::CfOnly => assert!(g.claim.as_ref().is_some_and(|c| c.r#ref.is_none())),
                _ => assert!(own.is_some()),
            }
        }
    }
}

fn ref_of(g: &Group) -> Option<String> {
    g.claim
        .as_ref()
        .and_then(|c| c.r#ref.as_ref())
        .map(Ref::to_string)
}

fn names(g: &Group) -> Vec<&str> {
    g.folders.iter().map(|f| f.as_str()).collect()
}

fn alternatives(g: &Group) -> Vec<String> {
    g.alternatives.iter().map(Ref::to_string).collect()
}

fn evidence_of(g: &Group) -> Vec<Evidence> {
    g.claim
        .as_ref()
        .map(|c| c.evidence.clone())
        .unwrap_or_default()
}

fn synthetic(folders: &[(&str, &str)]) -> Vec<(String, String)> {
    folders
        .iter()
        .map(|(f, toc)| ((*f).to_owned(), (*toc).to_owned()))
        .collect()
}

#[test]
fn bigwigs_goes_to_the_catalog_with_its_wowi_id_as_an_alternative() {
    let env = TestEnv::new();
    let client = Client::new(&env).with(&set("bigwigs"));
    let groups = groups(&client.candidates(), &Cat::builtin(), None);
    let [g] = groups.as_slice() else {
        unreachable!("{groups:#?}")
    };
    assert_eq!(ref_of(g).as_deref(), Some("gh:BigWigsMods/BigWigs"));
    assert_eq!(g.tier, Tier::Identified);
    assert_eq!(
        g.claim.as_ref().map(|c| c.strength),
        Some(Strength::Catalog)
    );
    assert_eq!(
        evidence_of(g),
        [Evidence::CatalogFolder, Evidence::TocWowiId]
    );
    assert_eq!(alternatives(g), ["wowi:5086"]);
    assert_eq!(
        names(g),
        [
            "BigWigs",
            "BigWigs_Core",
            "BigWigs_Options",
            "BigWigs_Plugins"
        ]
    );
    assert_eq!(g.primary.as_str(), "BigWigs");
    assert_eq!(
        g.claim.as_ref().and_then(|c| c.name.as_deref()),
        Some("BigWigs")
    );
}

#[test]
fn toc_ids_and_websites_identify_by_themselves() {
    let env = TestEnv::new();
    let client = Client::new(&env).with(&synthetic(&[
        ("Foo", "## Interface: 120105\n## X-WoWI-ID: 777\n"),
        (
            "Hub",
            "## Interface: 120105\n## X-Website: https://github.com/o/r\n",
        ),
        (
            "Dbmish",
            "## Interface: 120105\n## X-Website: https://deadlybossmods.com\n",
        ),
    ]));
    let groups = groups(&client.candidates(), &Cat::only(Vec::new()), None);
    let found: Vec<(Option<String>, Tier, Option<Strength>)> = groups
        .iter()
        .map(|g| (ref_of(g), g.tier, g.claim.as_ref().map(|c| c.strength)))
        .collect();
    assert_eq!(
        found,
        [
            (None, Tier::Unknown, None),
            (
                Some("wowi:777".into()),
                Tier::Identified,
                Some(Strength::TocId)
            ),
            (
                Some("gh:o/r".into()),
                Tier::Identified,
                Some(Strength::TocId)
            ),
        ],
        "deadlybossmods.com names no source, and never becomes url:"
    );
}

#[test]
fn elvui_is_one_tukui_group_and_a_catalog_entry_makes_it_stronger() {
    let env = TestEnv::new();
    let client = Client::new(&env).with(&set("elvui"));
    let candidates = client.candidates();
    for (catalog, strength) in [
        (Cat::only(Vec::new()), Strength::TocId),
        (
            Cat::only(vec![entry(
                "ElvUI",
                &[],
                "tukui:elvui",
                &[],
                &["ElvUI", "ElvUI_Libraries", "ElvUI_Options"],
            )]),
            Strength::Catalog,
        ),
    ] {
        let groups = groups(&candidates, &catalog, None);
        let [g] = groups.as_slice() else {
            unreachable!("{groups:#?}")
        };
        assert_eq!(ref_of(g).as_deref(), Some("tukui:elvui"));
        assert_eq!(names(g), ["ElvUI", "ElvUI_Libraries", "ElvUI_Options"]);
        assert_eq!(
            (g.tier, g.claim.as_ref().map(|c| c.strength)),
            (Tier::Identified, Some(strength))
        );
        assert_eq!(g.primary.as_str(), "ElvUI");
    }
}

#[test]
fn dbm_is_one_group_and_only_a_same_prefix_stray_joins() {
    let env = TestEnv::new();
    let client = Client::new(&env).with(&set("dbm"));
    let groups_alone = groups(&client.candidates(), &Cat::builtin(), None);
    let [g] = groups_alone.as_slice() else {
        unreachable!("{groups_alone:#?}")
    };
    assert_eq!(
        (
            ref_of(g).as_deref(),
            g.tier,
            g.folders.len(),
            g.primary.as_str()
        ),
        (
            Some("gh:DeadlyBossMods/DeadlyBossMods"),
            Tier::Identified,
            8,
            "DBM-Core"
        )
    );

    let client = client.with(&set("dbm_strays"));
    let groups = groups(&client.candidates(), &Cat::builtin(), None);
    let found: Vec<(Option<String>, Tier, usize)> = groups
        .iter()
        .map(|g| (ref_of(g), g.tier, g.folders.len()))
        .collect();
    assert_eq!(
        found,
        [
            (
                Some("gh:DeadlyBossMods/DeadlyBossMods".into()),
                Tier::Identified,
                9
            ),
            (None, Tier::Unknown, 1),
        ],
        "DBM-Extra joins without lowering the tier; XYZ-Thing does not"
    );
    assert!(groups[0].folders.iter().any(|f| f.as_str() == "DBM-Extra"));
    assert_eq!(groups[1].primary.as_str(), "XYZ-Thing");
}

#[test]
fn a_uidir_match_is_identified_only_when_corroborated() {
    let wowi = Wowi(
        [project(
            100,
            "|cff00ff00Bagnon|r Bags",
            "Jaliborc",
            &["Bagnon", "__MACOSX"],
            9,
        )]
        .into(),
    );
    let catalog = Cat::only(Vec::new());
    for (toc, tier, strength) in [
        (
            "## Interface: 120105\n## Title: |cffff0000Bagnon|r\n",
            Tier::Identified,
            Strength::Corroborated,
        ),
        (
            "## Interface: 120105\n## Title: Bags\n## Author: Jaliborc\n",
            Tier::Identified,
            Strength::Corroborated,
        ),
        (
            "## Interface: 120105\n## Title: Other\n## Author: Someone\n",
            Tier::Guess,
            Strength::NameOnly,
        ),
    ] {
        let env = TestEnv::new();
        let client = Client::new(&env).with(&synthetic(&[("Bagnon", toc)]));
        let groups = groups(&client.candidates(), &catalog, Some(&wowi));
        let [g] = groups.as_slice() else {
            unreachable!("{groups:#?}")
        };
        assert_eq!(ref_of(g).as_deref(), Some("wowi:100"), "{toc}");
        assert_eq!(
            (g.tier, g.claim.as_ref().map(|c| c.strength)),
            (tier, Some(strength)),
            "{toc}"
        );
    }
}

#[test]
fn name_only_and_curse_only_folders_are_guesses_and_cf_only() {
    let env = TestEnv::new();
    let client = Client::new(&env).with(&synthetic(&[
        ("WA", "## Interface: 120105\n"),
        (
            "TSM",
            "## Interface: 120105\n## X-Curse-Project-ID: 12345\n",
        ),
        ("MyMacros", "## Interface: 120105\n## Title: My Macros\n"),
    ]));
    let catalog = Cat::only(vec![entry(
        "WeakAuras",
        &["wa"],
        "gh:WeakAuras/WeakAuras2",
        &[Family::Vanilla],
        &["WeakAuras", "WeakAurasOptions"],
    )]);
    let groups = groups(&client.candidates(), &catalog, None);
    type Row<'a> = (Vec<&'a str>, Option<String>, Tier, Vec<Evidence>);
    let found: Vec<Row<'_>> = groups
        .iter()
        .map(|g| (names(g), ref_of(g), g.tier, evidence_of(g)))
        .collect();
    assert_eq!(
        found,
        [
            (vec!["MyMacros"], None, Tier::Unknown, vec![]),
            (vec!["TSM"], None, Tier::CfOnly, vec![Evidence::TocCurseId]),
            (
                vec!["WA"],
                Some("gh:WeakAuras/WeakAuras2".into()),
                Tier::Guess,
                vec![Evidence::NameMatch]
            ),
        ]
    );
}

#[test]
fn equal_claims_make_a_guess_with_the_other_as_alternative() {
    // Two catalog entries list one folder.
    let env = TestEnv::new();
    let client = Client::new(&env).with(&synthetic(&[("Shared", "## Interface: 120105\n")]));
    let catalog = Cat::only(vec![
        entry("Alpha", &[], "gh:a/alpha", &[], &["Shared"]),
        entry("Beta", &[], "gh:b/beta", &[], &["Shared"]),
    ]);
    let groups_two = groups(&client.candidates(), &catalog, None);
    let [g] = groups_two.as_slice() else {
        unreachable!("{groups_two:#?}")
    };
    assert_eq!(
        (ref_of(g).as_deref(), g.tier, alternatives(g)),
        (
            Some("gh:a/alpha"),
            Tier::Guess,
            vec!["gh:b/beta".to_owned()]
        )
    );

    // Two WoWInterface projects share `Details`: the one covering more of what is
    // present wins, contested.
    let env = TestEnv::new();
    let client = Client::new(&env).with(&set("details"));
    let wowi = Wowi(
        [
            project(
                23056,
                "Details! Damage Meter",
                "Terciob",
                &["Details", "Details_DataStorage"],
                50,
            ),
            project(
                999,
                "Details Fork",
                "Someone",
                &["Details", "Details_Other"],
                500,
            ),
        ]
        .into(),
    );
    let groups = groups(&client.candidates(), &Cat::only(Vec::new()), Some(&wowi));
    let [g] = groups.as_slice() else {
        unreachable!("{groups:#?}")
    };
    assert_eq!(ref_of(g).as_deref(), Some("wowi:23056"));
    assert_eq!(g.tier, Tier::Guess);
    assert_eq!(alternatives(g), ["wowi:999"]);
    assert_eq!(names(g), ["Details", "Details_DataStorage"]);
}

#[test]
fn bugsack_goes_to_its_catalog_entry_with_its_wowi_id_as_alternative() {
    let env = TestEnv::new();
    let client = Client::new(&env).with(&set("bugsack"));
    let catalog = Cat::only(vec![entry(
        "BugSack",
        &[],
        "gh:funkydude/BugSack",
        &[],
        &["BugSack"],
    )]);
    let groups = groups(&client.candidates(), &catalog, None);
    let [g] = groups.as_slice() else {
        unreachable!("{groups:#?}")
    };
    assert_eq!(ref_of(g).as_deref(), Some("gh:funkydude/BugSack"));
    assert_eq!(g.tier, Tier::Identified);
    assert_eq!(alternatives(g), ["wowi:5995"]);
    assert_eq!(
        evidence_of(g),
        [Evidence::CatalogFolder, Evidence::TocWowiId]
    );
    assert!(
        !evidence_of(g)
            .iter()
            .any(|e| matches!(e, Evidence::TocCurseId | Evidence::TocWagoId))
    );
}

#[test]
fn alternatives_are_capped() {
    let env = TestEnv::new();
    let client = Client::new(&env).with(&synthetic(&[("Common", "## Interface: 120105\n")]));
    let wowi = Wowi(
        (1..=5)
            .map(|uid| {
                project(
                    uid,
                    &format!("Pack {uid}"),
                    "x",
                    &["Common", &format!("Other{uid}")],
                    uid,
                )
            })
            .collect(),
    );
    let groups = groups(&client.candidates(), &Cat::only(Vec::new()), Some(&wowi));
    let [g] = groups.as_slice() else {
        unreachable!("{groups:#?}")
    };
    assert_eq!(g.tier, Tier::Guess);
    assert_eq!(g.alternatives.len(), 3, "{g:#?}");
}

#[test]
fn every_fixture_folder_together_keeps_the_invariants() {
    let env = TestEnv::new();
    let mut client = Client::new(&env);
    for name in [
        "dbm",
        "dbm_strays",
        "bigwigs",
        "elvui",
        "bugsack",
        "details",
    ] {
        client = client.with(&set(name));
    }
    let wowi = Wowi(
        [
            project(
                5086,
                "BigWigs Bossmods",
                "Funkeh",
                &["BigWigs", "BigWigs_Core"],
                10,
            ),
            project(5995, "BugSack", "funkydude", &["BugSack"], 10),
            project(
                23056,
                "Details! Damage Meter",
                "Terciob",
                &["Details", "Details_DataStorage", ""],
                10,
            ),
        ]
        .into(),
    );
    let all = client.candidates();
    let groups = groups(&all, &Cat::builtin(), Some(&wowi));
    let found: Vec<(String, Option<String>, Tier)> = groups
        .iter()
        .map(|g| (g.primary.as_str().to_owned(), ref_of(g), g.tier))
        .collect();
    assert_eq!(
        found,
        [
            (
                "BigWigs".into(),
                Some("gh:BigWigsMods/BigWigs".into()),
                Tier::Identified
            ),
            // The built-in catalog names BugSack's GitHub home over its TOC's WoWI id.
            (
                "BugSack".into(),
                Some("gh:funkydude/BugSack".into()),
                Tier::Identified,
            ),
            (
                "DBM-Core".into(),
                Some("gh:DeadlyBossMods/DeadlyBossMods".into()),
                Tier::Identified
            ),
            // Details' title corroborates, but Details_DataStorage's own UIDir match
            // does not (its title is "Details! Storage"): the weakest member decides.
            ("Details".into(), Some("wowi:23056".into()), Tier::Guess),
            ("ElvUI".into(), Some("tukui:elvui".into()), Tier::Identified),
            ("XYZ-Thing".into(), None, Tier::Unknown),
        ]
    );
}

proptest::proptest! {
    #![proptest_config(proptest::prelude::ProptestConfig::with_cases(64))]

    #[test]
    fn any_subset_in_any_order_keeps_the_invariants(
        picks in proptest::collection::vec(proptest::prelude::any::<bool>(), 22),
        seed in proptest::prelude::any::<u64>(),
    ) {
        let env = TestEnv::new();
        let mut client = Client::new(&env);
        for name in ["dbm", "dbm_strays", "bigwigs", "elvui", "bugsack", "details"] {
            client = client.with(&set(name));
        }
        let wowi = Wowi(
            [
                project(5086, "BigWigs Bossmods", "Funkeh", &["BigWigs", "BigWigs_Core"], 10),
                project(23056, "Details! Damage Meter", "Terciob", &["Details", "Details_DataStorage"], 10),
                project(23057, "Details Lite", "Someone", &["Details"], 20),
            ]
            .into(),
        );
        let all = client.candidates();
        let mut subset: Vec<_> = all
            .into_iter()
            .zip(picks.iter().cycle())
            .filter(|(_, keep)| **keep)
            .map(|(c, _)| c)
            .collect();
        // A seeded shuffle.
        let len = subset.len();
        if len > 1 {
            let mut state = seed;
            for i in (1..len).rev() {
                state = state.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                let j = usize::try_from(state >> 33).unwrap_or(0) % (i + 1);
                subset.swap(i, j);
            }
        }
        let _ = groups(&subset, &Cat::builtin(), Some(&wowi));
    }
}
