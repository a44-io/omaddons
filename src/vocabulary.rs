//! The words a user may type for a client (M4-D12): every built-in name the rules table
//! knows (default ids, product codes, flavor folders, game modes and their aliases, family
//! names) and the ids `registry.json` holds, in `-t`'s resolution order. The usage path
//! uses it to recognise a client named where a command or an argument belongs
//! (`omaddons retail update`), and moves it into `-t`.
//!
//! It reads at most one file, `registry.json`, bounded by `DISCOVERY_FILE_MAX_BYTES` and
//! read-only, and never runs discovery: a usage error must stay fast and side-effect free.

use std::ffi::OsString;

use crate::rules::{Family, RulesTable};
use crate::state::paths::Paths;
use crate::state::registry::Registry;
use crate::state::store::{self, VersionStatus};

/// What a client word names, in `-t`'s resolution order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TargetWordKind {
    /// A registered target's id.
    Registered,
    /// A product's default id, or a family's custom-client default id (`wotlk`).
    DefaultId,
    /// A Battle.net product code (`wow_classic_era`).
    ProductCode,
    /// A flavor folder (`_classic_era_`).
    FlavorFolder,
    /// A game mode's id or alias (`sod`, `hc`).
    Mode,
    /// A family name (`wrath-legacy`).
    Family,
}

/// One client word: its kind, and the spelling `-t` resolves it as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetWord {
    pub kind: TargetWordKind,
    pub canonical: String,
}

/// Every client word, each spelling once, in resolution order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TargetVocabulary {
    /// (lowercase spelling, word).
    words: Vec<(String, TargetWord)>,
}

impl TargetVocabulary {
    /// No words: the behaviour before M4-D12's hooks; the tests' oracle.
    pub fn empty() -> Self {
        Self::default()
    }

    /// The built-in words alone.
    pub fn builtin() -> Self {
        Self::new(&RulesTable::builtin(), [])
    }

    /// The words of `table` and the `registered` ids, a spelling taken by the first kind
    /// that names it: registered ids, default ids, product codes, flavor folders, game
    /// modes, families. Keys and paths are not words.
    pub fn new<'a>(table: &RulesTable, registered: impl IntoIterator<Item = &'a str>) -> Self {
        let mut vocabulary = Self::default();
        for id in registered {
            vocabulary.push(TargetWordKind::Registered, id, id);
        }
        for product in table.products() {
            vocabulary.push(
                TargetWordKind::DefaultId,
                &product.default_id,
                &product.default_id,
            );
        }
        for family in Family::ALL {
            if let Some(id) = table.default_id_for_family(family) {
                vocabulary.push(TargetWordKind::DefaultId, id, &id.to_ascii_lowercase());
            }
        }
        for product in table.products() {
            vocabulary.push(TargetWordKind::ProductCode, &product.code, &product.code);
        }
        for product in table.products() {
            vocabulary.push(
                TargetWordKind::FlavorFolder,
                &product.folder,
                &product.folder,
            );
        }
        for product in table.products() {
            for mode in &product.modes {
                vocabulary.push(TargetWordKind::Mode, &mode.id, &mode.id);
                for alias in &mode.aliases {
                    vocabulary.push(TargetWordKind::Mode, alias, &mode.id);
                }
            }
        }
        for family in Family::ALL {
            vocabulary.push(TargetWordKind::Family, family.as_str(), family.as_str());
        }
        vocabulary
    }

    /// The built-in words and the ids of `paths`' `registry.json`, read bounded and
    /// read-only. A missing, unreadable, oversized or undecodable registry gives the
    /// built-in words alone, and so does one a newer omaddons wrote (its meaning is not
    /// this build's to interpret). Nothing is reported: the command the fix runs reports
    /// a damaged registry itself.
    pub fn load(paths: &Paths) -> Self {
        let path = paths.registry_file();
        let ids: Vec<String> = crate::discover::read_bytes(&path)
            .and_then(|bytes| store::load_bytes::<Registry>(&path, &bytes).ok())
            .filter(|loaded| loaded.status != VersionStatus::Newer)
            .map(|loaded| loaded.value.targets.into_iter().map(|e| e.id).collect())
            .unwrap_or_default();
        Self::new(&RulesTable::builtin(), ids.iter().map(String::as_str))
    }

    /// [`Self::load`] from the environment's paths; the built-in words when they cannot
    /// be told (no `HOME`).
    pub fn from_env(env: &dyn Fn(&str) -> Option<String>) -> Self {
        Paths::from_lookup(|name| env(name).map(OsString::from))
            .map_or_else(|_| Self::builtin(), |paths| Self::load(&paths))
    }

    /// The client `word` names, ignoring ASCII case; `None` for anything else.
    pub fn classify(&self, word: &str) -> Option<TargetWord> {
        let spelling = word.to_ascii_lowercase();
        self.words
            .iter()
            .find(|(known, _)| *known == spelling)
            .map(|(_, word)| word.clone())
    }

    /// The words that may be what `typed` meant, a selector that matched nothing (M4-D14),
    /// best first: within Levenshtein distance 2 of `typed` (ASCII case ignored) and closer
    /// than its length, never equal to it, of kind Registered, DefaultId, Mode or Family,
    /// and accepted by `admit`. Ordered by distance, then registered ids before built-in
    /// words, then alphabetically. A registered id is returned as stored, any other word as
    /// the vocabulary spells it (`hc`, not `hardcore`). Pure: no I/O.
    pub fn closest(&self, typed: &str, admit: &dyn Fn(&str) -> bool) -> Vec<String> {
        let typed = typed.to_ascii_lowercase();
        let len = typed.chars().count();
        let mut close: Vec<(usize, bool, String)> = Vec::new();
        for (spelling, word) in &self.words {
            // Product codes and flavor folders are spellings copied from output; a near
            // miss of one is near an id or mode that selects the same client.
            if matches!(
                word.kind,
                TargetWordKind::ProductCode | TargetWordKind::FlavorFolder
            ) {
                continue;
            }
            // The distance is at least the length difference: skipping first bounds the
            // work by the vocabulary, whatever length was typed.
            if spelling.chars().count().abs_diff(len) > 2 {
                continue;
            }
            let distance = crate::cli::levenshtein(&typed, spelling);
            if !(1..=2).contains(&distance) || distance >= len {
                continue;
            }
            let registered = matches!(word.kind, TargetWordKind::Registered);
            let returned = if registered {
                word.canonical.clone()
            } else {
                spelling.clone()
            };
            if admit(&returned) {
                close.push((distance, !registered, returned));
            }
        }
        close.sort();
        close.into_iter().map(|(_, _, word)| word).collect()
    }

    /// Every word, in resolution order.
    pub fn words(&self) -> impl Iterator<Item = (&str, &TargetWord)> {
        self.words
            .iter()
            .map(|(spelling, word)| (spelling.as_str(), word))
    }

    /// Adds `spelling` as a `kind` word for `canonical`, unless a word already has it.
    fn push(&mut self, kind: TargetWordKind, spelling: &str, canonical: &str) {
        let spelling = spelling.to_ascii_lowercase();
        if spelling.is_empty() || self.words.iter().any(|(known, _)| *known == spelling) {
            return;
        }
        let word = TargetWord {
            kind,
            canonical: canonical.to_owned(),
        };
        self.words.push((spelling, word));
    }
}

/// First words of command paths whose run selects clients with -t: a misplaced client
/// before or after one of them is moved into -t (M4-D12).
pub const SELECTS_CLIENT: &[&str] = &[
    "status", "list", "outdated", "info", "search", "errors", "export", "history", "install",
    "update", "pin", "unpin", "adopt", "sync", "remove", "undo", "backup",
];

/// First words of command paths whose run never reads -t (or refuses it: copy): a
/// misplaced client keeps the fix it had before M4-D12.
pub const CLIENT_FREE: &[&str] = &[
    "help", "config", "targets", "net-log", "auth", "omarchy", "copy",
];

/// Whether a misplaced client may move into -t on the command at `path`.
pub fn selects_client(path: &[String]) -> bool {
    path.first()
        .is_some_and(|first| SELECTS_CLIENT.contains(&first.as_str()))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use jiff::Timestamp;

    use super::*;
    use crate::model::TargetKey;
    use crate::state::registry::{Origin, RegistryEntry};

    fn check(
        vocabulary: &TargetVocabulary,
        case: &str,
        word: &str,
        want: Option<(TargetWordKind, &str)>,
    ) {
        let got = vocabulary.classify(word);
        let want = want.map(|(kind, canonical)| TargetWord {
            kind,
            canonical: canonical.to_owned(),
        });
        assert_eq!(
            got, want,
            "{case}: classify({word:?}) = {got:?}, expected {want:?}"
        );
    }

    #[test]
    fn classify_names_every_builtin_kind_in_resolution_order() {
        use TargetWordKind::{DefaultId, Family, FlavorFolder, Mode, ProductCode};
        let builtin = TargetVocabulary::builtin();
        let cases: &[(&str, &str, TargetWordKind, &str)] = &[
            ("a default id, not the mode", "retail", DefaultId, "retail"),
            ("any case", "RETAIL", DefaultId, "retail"),
            ("era", "era", DefaultId, "era"),
            ("classic-ptr", "classic-ptr", DefaultId, "classic-ptr"),
            (
                "a product, not the mode or family",
                "forever",
                DefaultId,
                "forever",
            ),
            (
                "a family row's custom-client id",
                "wotlk",
                DefaultId,
                "wotlk",
            ),
            ("vanilla, not the family", "vanilla", DefaultId, "vanilla"),
            ("mainline", "mainline", DefaultId, "mainline"),
            (
                "a product code",
                "wow_classic_era",
                ProductCode,
                "wow_classic_era",
            ),
            ("a product code in capitals", "WOW", ProductCode, "wow"),
            (
                "a flavor folder",
                "_classic_era_",
                FlavorFolder,
                "_classic_era_",
            ),
            (
                "a flavor folder in mixed case",
                "_Retail_",
                FlavorFolder,
                "_retail_",
            ),
            ("a mode", "sod", Mode, "sod"),
            ("a mode alias", "Season-of-Discovery", Mode, "sod"),
            ("hc", "hc", Mode, "hardcore"),
            ("mop", "mop", Mode, "mists-classic"),
            ("classic-era", "classic-era", Mode, "classic-era"),
            ("bc-anniversary", "bc-anniversary", Mode, "tbc-anniversary"),
            ("a family", "wrath-legacy", Family, "wrath-legacy"),
            ("unknown is a family", "Unknown", Family, "unknown"),
        ];
        for (case, word, kind, canonical) in cases {
            check(&builtin, case, word, Some((*kind, canonical)));
        }
    }

    #[test]
    fn classify_names_nothing_else() {
        let builtin = TargetVocabulary::builtin();
        for word in [
            "dbm",
            "questie",
            "weakauras",
            "update",
            "status",
            "rm",
            "",
            " retail",
            "retail ",
            "re tail",
            "rétail",
            "classic_era",
            "retail@steam",
            "turtle",
            "01JZ0000000000000000000000",
            "/x",
            "-t",
        ] {
            check(&builtin, "not a client", word, None);
        }
        assert_eq!(TargetVocabulary::empty().classify("retail"), None, "empty");
    }

    #[test]
    fn registered_ids_are_client_words_and_come_first() {
        use TargetWordKind::{Mode, Registered};
        let vocabulary =
            TargetVocabulary::new(&RulesTable::builtin(), ["turtle", "sod", "retail@steam"]);
        check(
            &vocabulary,
            "a registered id",
            "turtle",
            Some((Registered, "turtle")),
        );
        check(
            &vocabulary,
            "beats a mode",
            "sod",
            Some((Registered, "sod")),
        );
        check(
            &vocabulary,
            "any case",
            "RETAIL@STEAM",
            Some((Registered, "retail@steam")),
        );
        check(
            &vocabulary,
            "other modes stay",
            "hc",
            Some((Mode, "hardcore")),
        );
    }

    #[test]
    fn every_spelling_appears_once() {
        let builtin = TargetVocabulary::builtin();
        let spellings: Vec<&str> = builtin.words().map(|(spelling, _)| spelling).collect();
        let mut unique = spellings.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), spellings.len(), "{spellings:?}");
        for spelling in spellings {
            assert_eq!(spelling, spelling.to_ascii_lowercase(), "{spelling}");
        }
    }

    fn entry(root: &Path, id: &str) -> RegistryEntry {
        RegistryEntry {
            key: TargetKey::new(),
            id: id.to_owned(),
            install_root: root.join("wow"),
            client_dir: root.join("wow/_retail_"),
            flavor_dir: Some("_retail_".to_owned()),
            product: None,
            qualifier: String::new(),
            origin: Origin::Manual,
            registered_at: Timestamp::UNIX_EPOCH,
        }
    }

    #[test]
    fn load_reads_the_registry_bounded_and_read_only() {
        use TargetWordKind::{DefaultId, Registered};
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let paths = Paths::with_roots(root, root.join("c"), root.join("s"), root.join("k"));
        let registry = paths.registry_file();
        std::fs::create_dir_all(registry.parent().unwrap()).unwrap();
        let valid = store::encode(&Registry {
            targets: vec![entry(root, "turtle")],
            default: None,
        })
        .unwrap();
        let text = String::from_utf8(valid.clone()).unwrap();
        let mut oversized = valid.clone();
        oversized.resize(crate::limits::DISCOVERY_FILE_MAX_BYTES as usize + 1, b' ');
        let newer = text.replacen(
            "\"schema_version\": \"1.0\"",
            "\"schema_version\": \"9.0\"",
            1,
        );
        assert_ne!(newer, text, "the registry names its version");
        let cases: Vec<(&str, Option<Vec<u8>>, bool)> = vec![
            ("missing", None, false),
            ("valid", Some(valid), true),
            ("unparsable", Some(b"{ not json".to_vec()), false),
            ("directory", None, false),
            ("oversized", Some(oversized), false),
            ("newer", Some(newer.into_bytes()), false),
        ];
        for (case, bytes, registered) in cases {
            let _ = std::fs::remove_file(&registry);
            let _ = std::fs::remove_dir(&registry);
            match (&bytes, case) {
                (Some(bytes), _) => std::fs::write(&registry, bytes).unwrap(),
                (None, "directory") => std::fs::create_dir(&registry).unwrap(),
                (None, _) => {}
            }
            let vocabulary = TargetVocabulary::load(&paths);
            let want = registered.then_some((Registered, "turtle"));
            check(&vocabulary, case, "turtle", want);
            check(&vocabulary, case, "retail", Some((DefaultId, "retail")));
            if let Some(bytes) = bytes {
                assert_eq!(
                    std::fs::read(&registry).unwrap(),
                    bytes,
                    "{case}: unchanged"
                );
            }
        }
    }

    #[test]
    fn from_env_without_a_home_is_the_builtin_vocabulary() {
        assert_eq!(
            TargetVocabulary::from_env(&|_| None),
            TargetVocabulary::builtin()
        );
    }

    #[test]
    fn every_command_says_whether_it_selects_a_client() {
        let mut firsts: Vec<&str> = crate::app::COMMANDS
            .iter()
            .filter_map(|c| c.name.split(' ').next())
            .collect();
        firsts.sort_unstable();
        firsts.dedup();
        for word in &firsts {
            let lists = usize::from(SELECTS_CLIENT.contains(word))
                + usize::from(CLIENT_FREE.contains(word));
            assert_eq!(
                lists, 1,
                "`{word}` is a command in app::COMMANDS but in neither vocabulary::SELECTS_CLIENT nor vocabulary::CLIENT_FREE (or in both): add it to SELECTS_CLIENT when its run reads -t, else to CLIENT_FREE (a misplaced client before or after it then keeps today's fix)"
            );
        }
        for word in SELECTS_CLIENT.iter().chain(CLIENT_FREE) {
            assert!(
                firsts.contains(word),
                "`{word}` is no first word of app::COMMANDS"
            );
        }
        let path = |words: &[&str]| words.iter().map(|w| (*w).to_owned()).collect::<Vec<_>>();
        assert!(selects_client(&path(&["update"])));
        assert!(selects_client(&path(&["backup", "create"])));
        assert!(!selects_client(&path(&["copy"])));
        assert!(!selects_client(&path(&[])));
    }

    fn check_closest(case: &str, vocabulary: &TargetVocabulary, typed: &str, want: &[&str]) {
        let got = vocabulary.closest(typed, &|_| true);
        assert_eq!(
            got, want,
            "{case}: closest({typed:?}) = {got:?}, expected {want:?}"
        );
    }

    #[test]
    fn closest_ranks_by_distance_then_registered_ids_then_alphabetically() {
        let builtin = TargetVocabulary::builtin();
        let table = RulesTable::builtin();
        let zera = TargetVocabulary::new(&table, ["zera"]);
        let sad = TargetVocabulary::new(&table, ["sad"]);
        for (case, vocabulary, typed, want) in [
            ("one_transposition", &builtin, "retial", &["retail"][..]),
            ("case_is_ignored", &builtin, "RETIAL", &["retail"]),
            ("nearest_first", &builtin, "ptx", &["ptr", "xptr"]),
            ("a_mode_alias_as_spelled", &builtin, "hcc", &["hc", "tbc"]),
            (
                "alphabetical_among_equals",
                &builtin,
                "bera",
                &["beta", "era"],
            ),
            (
                "registered_first_among_equals",
                &zera,
                "bera",
                &["zera", "beta", "era"],
            ),
            ("registered_id_against_a_mode", &sad, "sxd", &["sad", "sod"]),
        ] {
            check_closest(case, vocabulary, typed, want);
        }
    }

    #[test]
    fn closest_never_offers_codes_folders_the_word_itself_or_a_wild_guess() {
        let builtin = TargetVocabulary::builtin();
        let huge = "x".repeat(100_000);
        for (case, typed, want) in [
            ("product_code", "wow_classic_ear", &[][..]),
            (
                "flavor_folder_but_its_mode",
                "_classic_era",
                &["classic-era"],
            ),
            ("flavor_folder_but_its_id", "_retail", &["retail"]),
            ("exact_word", "retail", &[]),
            ("one_letter", "x", &[]),
            ("two_letters", "er", &["era"]),
            ("empty", "", &[]),
            ("far", "zzzzzz", &[]),
            ("huge", huge.as_str(), &[]),
        ] {
            check_closest(case, &builtin, typed, want);
        }
    }

    #[test]
    fn closest_asks_admit_only_about_close_words() {
        let builtin = TargetVocabulary::builtin();
        let asked = std::cell::RefCell::new(Vec::new());
        let recording = |word: &str| {
            asked.borrow_mut().push(word.to_owned());
            true
        };
        assert_eq!(builtin.closest("retial", &recording), ["retail"]);
        assert_eq!(
            *asked.borrow(),
            ["retail"],
            "only the close words are asked"
        );
        let no_beta = |word: &str| word != "beta";
        assert_eq!(builtin.closest("bera", &no_beta), ["era"]);
        assert!(builtin.closest("bera", &|_| false).is_empty());
    }

    #[test]
    fn the_vocabulary_reads_nothing_but_the_registry() {
        let source = include_str!("vocabulary.rs");
        let production = source.split("#[cfg(test)]").next().unwrap();
        for forbidden in [
            "World::",
            "DiscoveryMode::",
            "load_registry(",
            "Catalog::load(",
            "read_dir(",
            "fs::read(",
            "File::open(",
            "fsx::",
        ] {
            assert!(
                !production.contains(forbidden),
                "the vocabulary must not use {forbidden}"
            );
        }
        assert_eq!(
            production.matches("read_bytes(").count(),
            1,
            "one bounded read of registry.json"
        );
    }
}
