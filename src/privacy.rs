//! Privacy (M3-D21): no output names an account, realm or character, on any channel.
//!
//! Every path below `WTF/Account/` names all three: `Account/<account>/<realm>/
//! <character>/SavedVariables/X.lua`. [`WtfPathRedactor`] learns those folder names from
//! a client's `WTF/Account/` listing (or an archive's entry names) and maps them to
//! ordinals. Progress, warnings, error details and import refusals pass a WTF path
//! through [`WtfPathRedactor::rel`]; free text (`errors --raw-details`) goes through
//! [`WtfPathRedactor::text`]. It fails closed: a name it did not learn, past
//! `REDACTOR_MAX_NAMES` or created after the listing, maps to `account ?`, `realm ?` or
//! `character ?`, never to itself. The names never leave the type: its `Debug` prints
//! counts, and it does not implement `Serialize`.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::limits::REDACTOR_MAX_NAMES;
use crate::state::paths::ClientPaths;

/// WoW's own files in `WTF/` and its account and character folders: never a name.
const FIXED_FILES: &[&str] = &[
    "Config.wtf",
    "config-cache.wtf",
    "bindings-cache.wtf",
    "macros-cache.txt",
    "layout-local.txt",
    "chat-cache.txt",
    "AddOns.txt",
    "cache.md5",
];

/// The kinds of file WoW writes in `WTF/`. A character name is letters only and a realm
/// folder carries no extension, so a component that is not a learned name and ends in
/// one of these is a file, never a name.
const FILE_TYPES: &[&str] = &["wtf", "txt", "lua", "bak", "md5"];

/// What a folder name below `WTF/Account/` stands for, by its depth.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    Account,
    Realm,
    Character,
}

impl Kind {
    /// The kind one level deeper, `None` below a character.
    fn next(self) -> Option<Self> {
        match self {
            Self::Account => Some(Self::Realm),
            Self::Realm => Some(Self::Character),
            Self::Character => None,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Account => "account",
            Self::Realm => "realm",
            Self::Character => "character",
        }
    }
}

/// Maps the account, realm and character folder names of one client (or archive) to
/// `account N`, `realm N` and `character N` in paths, and to `<account>`, `<realm>` and
/// `<character>` in free text.
pub struct WtfPathRedactor {
    /// Each kind's names, folded to lower case, with their 1-based ordinals in the sorted
    /// order of those names.
    names: BTreeMap<Kind, BTreeMap<String, usize>>,
    /// Every name in free text, longest first, with its placeholder; a name of two kinds
    /// takes the first kind's (account, then realm, then character).
    words: Vec<(Vec<char>, &'static str)>,
    /// The listing reached `REDACTOR_MAX_NAMES`: later names were not learned.
    truncated: bool,
}

impl WtfPathRedactor {
    /// Learns the names under `client`'s `WTF/Account/`: each account folder, the realm
    /// folders inside it (`SavedVariables` aside) and the character folders inside those,
    /// at most `REDACTOR_MAX_NAMES` in all. Unreadable folders teach nothing.
    pub fn for_client(client: &ClientPaths) -> Self {
        Self::listing(client, REDACTOR_MAX_NAMES)
    }

    /// Learns the names in `paths`, an archive's entry names relative to `WTF/` (a
    /// leading `WTF/` is accepted too), at most `REDACTOR_MAX_NAMES` in all.
    pub fn for_paths<'a>(paths: impl Iterator<Item = &'a str>) -> Self {
        let mut learner = Learner::new(REDACTOR_MAX_NAMES);
        for path in paths {
            let Some((_, below)) = split_account(path) else {
                continue;
            };
            let mut kind = Some(Kind::Account);
            for part in below.iter().filter(|p| !p.is_empty()) {
                let Some(k) = kind.filter(|_| !is_file(part)) else {
                    break;
                };
                learner.learn(k, part);
                kind = k.next();
            }
        }
        learner.finish()
    }

    /// Every spelling of `WTF/Account/` is read, as `errors` reads them. Account names are
    /// learned first, so realms and characters never crowd one out of the bound.
    fn listing(client: &ClientPaths, max: usize) -> Self {
        let mut learner = Learner::new(max);
        let accounts: Vec<(String, std::path::PathBuf)> =
            crate::discover::find_children(client.client_dir(), "WTF")
                .iter()
                .flat_map(|wtf| crate::discover::find_children(wtf, "Account"))
                .flat_map(|root| folders(&root))
                .collect();
        for (account, _) in &accounts {
            learner.learn(Kind::Account, account);
        }
        for (_, account_dir) in &accounts {
            for (realm, realm_dir) in folders(account_dir) {
                if realm.eq_ignore_ascii_case("SavedVariables") {
                    continue;
                }
                learner.learn(Kind::Realm, &realm);
                for (character, _) in folders(&realm_dir) {
                    learner.learn(Kind::Character, &character);
                }
            }
        }
        learner.finish()
    }

    /// `rel`, a path relative to `WTF/` (a leading `WTF/` is accepted too), with each
    /// account, realm and character component replaced by its ordinal, `… ?` for one not
    /// learned: `Account/FAKEACCT/FakeRealm/FakeChar/SavedVariables/X.lua` becomes
    /// `Account/account 1/realm 1/character 1/SavedVariables/X.lua`. Components from
    /// `SavedVariables` or a WoW file on are kept; a path outside `Account/` is returned
    /// unchanged.
    pub fn rel(&self, rel: &str) -> String {
        let Some((head, below)) = split_account(rel) else {
            return rel.to_owned();
        };
        let mut out: Vec<String> = head.iter().map(|p| (*p).to_owned()).collect();
        let mut kind = Some(Kind::Account);
        for part in below {
            match kind {
                Some(k) if !part.is_empty() => {
                    if let Some(n) = self.ordinal(k, part) {
                        out.push(format!("{} {n}", k.label()));
                    } else if is_file(part) {
                        out.push(part.to_owned());
                        kind = None;
                        continue;
                    } else {
                        out.push(format!("{} ?", k.label()));
                    }
                    kind = k.next();
                }
                _ => out.push(part.to_owned()),
            }
        }
        out.join("/")
    }

    /// `s` with every learned name, as a whole word and ignoring case, replaced by
    /// `<account>`, `<realm>` or `<character>`. A name inside a longer word is left
    /// alone.
    pub fn text(&self, s: &str) -> String {
        let chars: Vec<char> = s.chars().collect();
        let mut out = String::with_capacity(s.len());
        let mut at = 0;
        while at < chars.len() {
            let before_is_word = at > 0 && is_word(chars[at - 1]);
            let found = (!before_is_word)
                .then(|| {
                    self.words.iter().find(|(word, _)| {
                        matches_at(&chars, at, word)
                            && chars.get(at + word.len()).is_none_or(|c| !is_word(*c))
                    })
                })
                .flatten();
            match found {
                Some((word, placeholder)) => {
                    out.push_str(placeholder);
                    at += word.len();
                }
                None => {
                    out.push(chars[at]);
                    at += 1;
                }
            }
        }
        out
    }

    /// The `N` of `account N` that [`Self::rel`] writes for the account folder `name`:
    /// `None` for a name not learned (`account ?`).
    pub fn account(&self, name: &str) -> Option<usize> {
        self.ordinal(Kind::Account, name)
    }

    fn ordinal(&self, kind: Kind, name: &str) -> Option<usize> {
        self.names.get(&kind)?.get(&fold(name)).copied()
    }
}

impl fmt::Debug for WtfPathRedactor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let count = |kind| self.names.get(&kind).map_or(0, BTreeMap::len);
        f.debug_struct("WtfPathRedactor")
            .field("accounts", &count(Kind::Account))
            .field("realms", &count(Kind::Realm))
            .field("characters", &count(Kind::Character))
            .field("truncated", &self.truncated)
            .finish()
    }
}

/// Names collected up to a bound.
struct Learner {
    names: BTreeMap<Kind, BTreeSet<String>>,
    left: usize,
    truncated: bool,
}

impl Learner {
    fn new(max: usize) -> Self {
        Self {
            names: BTreeMap::new(),
            left: max,
            truncated: false,
        }
    }

    fn learn(&mut self, kind: Kind, name: &str) {
        let folded = fold(name);
        let set = self.names.entry(kind).or_default();
        if set.contains(&folded) {
            return;
        }
        if self.left == 0 {
            self.truncated = true;
            return;
        }
        self.left -= 1;
        set.insert(folded);
    }

    fn finish(self) -> WtfPathRedactor {
        let names: BTreeMap<Kind, BTreeMap<String, usize>> = self
            .names
            .into_iter()
            .map(|(kind, set)| (kind, set.into_iter().zip(1..).collect()))
            .collect();
        let mut words: Vec<(Vec<char>, &'static str)> = Vec::new();
        for (kind, placeholder) in [
            (Kind::Account, "<account>"),
            (Kind::Realm, "<realm>"),
            (Kind::Character, "<character>"),
        ] {
            for name in names.get(&kind).into_iter().flat_map(BTreeMap::keys) {
                let word: Vec<char> = name.chars().collect();
                if !words.iter().any(|(w, _)| *w == word) {
                    words.push((word, placeholder));
                }
            }
        }
        // Longest first, so `Aerie Peak` wins over `Aerie`; stable within a length.
        words.sort_by_key(|(word, _)| std::cmp::Reverse(word.len()));
        WtfPathRedactor {
            names,
            words,
            truncated: self.truncated,
        }
    }
}

/// `path` split at its `Account` component (after an optional leading `WTF`): the
/// components up to it, and those below it. `None` outside `Account/`.
fn split_account(path: &str) -> Option<(Vec<&str>, Vec<&str>)> {
    let parts: Vec<&str> = path.split('/').collect();
    let skip = usize::from(parts.first().is_some_and(|p| p.eq_ignore_ascii_case("WTF")));
    let account = parts.get(skip)?;
    if !account.eq_ignore_ascii_case("Account") {
        return None;
    }
    let (head, below) = parts.split_at(skip + 1);
    Some((head.to_vec(), below.to_vec()))
}

/// `SavedVariables`, one of WoW's own files, or a file of a type WoW writes in `WTF/`.
fn is_file(part: &str) -> bool {
    part.eq_ignore_ascii_case("SavedVariables")
        || FIXED_FILES.iter().any(|f| f.eq_ignore_ascii_case(part))
        || part
            .rsplit_once('.')
            .is_some_and(|(_, ext)| FILE_TYPES.iter().any(|t| t.eq_ignore_ascii_case(ext)))
}

/// The folders in `dir` (a symlink to one counts), by name; nothing when unreadable.
fn folders(dir: &std::path::Path) -> Vec<(String, std::path::PathBuf)> {
    let Ok(listing) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<(String, std::path::PathBuf)> = listing
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_type()
                .is_ok_and(|t| t.is_dir() || t.is_symlink())
        })
        .map(|entry| {
            (
                entry.file_name().to_string_lossy().into_owned(),
                entry.path(),
            )
        })
        .collect();
    found.sort();
    found
}

/// A name's key: lower case, so `FakeRealm` and `fakerealm` are one name, as Wine sees
/// them. Folded one character to one character, as [`matches_at`] reads text: never
/// by context (a final `Σ` is `σ` like any other) nor into two (`İ` stays itself).
fn fold(name: &str) -> String {
    name.chars().map(fold_char).collect()
}

/// `c` in lower case when that is one character, else `c`.
fn fold_char(c: char) -> char {
    let mut lower = c.to_lowercase();
    match (lower.next(), lower.next()) {
        (Some(one), None) => one,
        _ => c,
    }
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Whether `word` (folded) starts at `chars[at]`, ignoring case.
fn matches_at(chars: &[char], at: usize, word: &[char]) -> bool {
    chars.len() >= at + word.len()
        && chars[at..at + word.len()]
            .iter()
            .zip(word)
            .all(|(c, w)| fold_char(*c) == *w)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const FOO: &str = "Account/FAKEACCT/FakeRealm/FakeChar/SavedVariables/Foo.lua";

    /// A client dir whose `WTF/Account/` holds `dirs` (paths below `Account/`).
    fn client(dirs: &[&str]) -> (tempfile::TempDir, ClientPaths) {
        let dir = tempfile::tempdir().unwrap();
        for rel in dirs {
            fs::create_dir_all(dir.path().join("WTF/Account").join(rel)).unwrap();
        }
        let client = ClientPaths::new(dir.path());
        (dir, client)
    }

    /// Every spelling of `WTF/Account/` is read, as `errors` reads them, and
    /// [`WtfPathRedactor::account`] gives the ordinal `rel` writes.
    #[test]
    fn every_spelling_of_the_account_folder_is_learned() {
        let (dir, c) = client(&["FAKEACCT/FakeRealm"]);
        fs::create_dir_all(dir.path().join("wtf/account/OTHERACCT/OtherRealm")).unwrap();
        let r = WtfPathRedactor::for_client(&c);
        assert_eq!(
            (r.account("FAKEACCT"), r.account("otheracct")),
            (Some(1), Some(2))
        );
        assert_eq!(
            r.rel("account/OTHERACCT/OtherRealm"),
            "account/account 2/realm 2"
        );
    }

    #[test]
    fn nested_paths_map_each_name_to_its_ordinal() {
        let (_dir, c) = client(&["FAKEACCT/FakeRealm/FakeChar/SavedVariables"]);
        let r = WtfPathRedactor::for_client(&c);
        assert_eq!(
            r.rel(FOO),
            "Account/account 1/realm 1/character 1/SavedVariables/Foo.lua"
        );
        assert_eq!(
            r.rel("Account/FAKEACCT/SavedVariables/Foo.lua"),
            "Account/account 1/SavedVariables/Foo.lua"
        );
        assert_eq!(
            r.rel("Account/FAKEACCT/FakeRealm"),
            "Account/account 1/realm 1",
            "a folder itself"
        );
        assert_eq!(
            r.rel(&format!("WTF/{FOO}")),
            "WTF/Account/account 1/realm 1/character 1/SavedVariables/Foo.lua",
            "a leading WTF/"
        );
    }

    #[test]
    fn two_accounts_share_a_realm_and_ordinals_follow_the_names() {
        let (_dir, c) = client(&["ZED/Realm/Alpha", "ABE/Realm/Beta", "ABE/Other/Gamma"]);
        let r = WtfPathRedactor::for_client(&c);
        assert_eq!(
            r.rel("Account/ABE/Realm/Beta"),
            "Account/account 1/realm 2/character 2"
        );
        assert_eq!(
            r.rel("Account/ZED/Realm/Alpha"),
            "Account/account 2/realm 2/character 1"
        );
        assert_eq!(
            r.rel("Account/ABE/Other/Gamma"),
            "Account/account 1/realm 1/character 3"
        );
    }

    #[test]
    fn a_character_named_like_a_realm_is_told_by_its_place() {
        let (_dir, c) = client(&["ACCT/Stormrage/Alice", "ACCT/Other/Stormrage"]);
        let r = WtfPathRedactor::for_client(&c);
        assert_eq!(
            r.rel("Account/ACCT/Other/Stormrage/SavedVariables/X.lua"),
            "Account/account 1/realm 1/character 2/SavedVariables/X.lua"
        );
        assert_eq!(
            r.rel("Account/ACCT/Stormrage/Alice"),
            "Account/account 1/realm 2/character 1"
        );
        assert_eq!(r.text("Stormrage"), "<realm>", "text takes the realm");
    }

    #[test]
    fn spellings_fold_case() {
        let (_dir, c) = client(&["FAKEACCT/FakeRealm/FakeChar"]);
        let r = WtfPathRedactor::for_client(&c);
        assert_eq!(
            r.rel("account/fakeacct/FAKEREALM/fakechar/savedvariables/foo.lua"),
            "account/account 1/realm 1/character 1/savedvariables/foo.lua"
        );
        assert_eq!(r.text("FAKECHAR and fakerealm"), "<character> and <realm>");
        // A spelling Wine would also find: `wtf/account/`.
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("wtf/account/Acct/Realm/Char")).unwrap();
        let r = WtfPathRedactor::for_client(&ClientPaths::new(dir.path()));
        assert_eq!(
            r.rel("Account/Acct/Realm/Char"),
            "Account/account 1/realm 1/character 1"
        );
    }

    #[test]
    fn paths_outside_account_are_unchanged() {
        let (_dir, c) = client(&["FAKEACCT/FakeRealm/FakeChar"]);
        let r = WtfPathRedactor::for_client(&c);
        for path in [
            "Config.wtf",
            "WTF/Config.wtf",
            "SavedVariables/Foo.lua",
            "",
            "Accounts/FAKEACCT",
        ] {
            assert_eq!(r.rel(path), path);
        }
    }

    #[test]
    fn text_replaces_whole_words_only() {
        let (_dir, c) = client(&["FAKEACCT/Aerie Peak/FakeChar", "FAKEACCT/Aerie/Other"]);
        let r = WtfPathRedactor::for_client(&c);
        assert_eq!(
            r.text("FakeChar-Aerie Peak: [FAKEACCT] FakeCharacter FakeChar_x fakechar."),
            "<character>-<realm>: [<account>] FakeCharacter FakeChar_x <character>."
        );
        assert_eq!(
            r.text("on Aerie, Aerie Peak"),
            "on <realm>, <realm>",
            "longest first"
        );
        assert_eq!(r.text(""), "");

        // A name whose lower case depends on context or is two characters long.
        let (_dir, c) = client(&["ΟΔΥΣΣΕΑΣ/İSTANBUL/Char"]);
        let r = WtfPathRedactor::for_client(&c);
        assert_eq!(
            r.text("ΟΔΥΣΣΕΑΣ on İSTANBUL, οδυσσεασ"),
            "<account> on <realm>, <account>"
        );
    }

    #[test]
    fn a_battlenet_account_is_a_name() {
        let (_dir, c) = client(&["123456#1/Realm/Char", "123456#2/SavedVariables"]);
        let r = WtfPathRedactor::for_client(&c);
        assert_eq!(
            r.rel("Account/123456#2/SavedVariables/X.lua"),
            "Account/account 2/SavedVariables/X.lua"
        );
        assert_eq!(
            r.text("account 123456#1 failed"),
            "account <account> failed"
        );
    }

    #[test]
    fn wow_files_and_saved_variables_are_never_names() {
        let (_dir, c) = client(&["FAKEACCT/SavedVariables", "FAKEACCT/FakeRealm/FakeChar"]);
        let r = WtfPathRedactor::for_client(&c);
        assert!(format!("{r:?}").contains("realms: 1"), "{r:?}");
        for file in FIXED_FILES {
            assert_eq!(
                r.rel(&format!("Account/FAKEACCT/{file}")),
                format!("Account/account 1/{file}")
            );
            assert_eq!(
                r.rel(&format!("Account/FAKEACCT/FakeRealm/FakeChar/{file}")),
                format!("Account/account 1/realm 1/character 1/{file}")
            );
        }
        assert_eq!(
            r.rel("Account/FAKEACCT/edit-mode-cache-account.txt"),
            "Account/account 1/edit-mode-cache-account.txt",
            "any WoW file type"
        );
    }

    #[test]
    fn names_it_did_not_learn_map_to_a_question_mark() {
        let (dir, c) = client(&["FAKEACCT/FakeRealm/FakeChar"]);
        let r = WtfPathRedactor::for_client(&c);
        fs::create_dir_all(dir.path().join("WTF/Account/NEWACCT/NewRealm/NewChar")).unwrap();
        assert_eq!(
            r.rel("Account/NEWACCT/NewRealm/NewChar/SavedVariables/X.lua"),
            "Account/account ?/realm ?/character ?/SavedVariables/X.lua",
            "created after the listing"
        );
        assert_eq!(
            r.rel("Account/FAKEACCT/NewRealm/FakeChar"),
            "Account/account 1/realm ?/character 1"
        );

        // Past the bound: every account first (`errors` numbers its databases by them),
        // then realms and characters while the bound lasts.
        let r = WtfPathRedactor::listing(&c, 2);
        assert!(format!("{r:?}").contains("truncated: true"), "{r:?}");
        assert_eq!(
            r.rel("Account/NEWACCT/NewRealm/NewChar"),
            "Account/account 2/realm ?/character ?"
        );
        assert_eq!(
            r.rel("Account/FAKEACCT/FakeRealm/FakeChar"),
            "Account/account 1/realm ?/character ?"
        );
        let r = WtfPathRedactor::listing(&c, 3);
        assert_eq!(
            r.rel("Account/FAKEACCT/FakeRealm/FakeChar"),
            "Account/account 1/realm 1/character ?"
        );
        assert_eq!(
            r.text("NewChar"),
            "NewChar",
            "text replaces only what it learned"
        );
    }

    #[test]
    fn archive_entry_names_teach_their_folders() {
        let entries = [
            "Account/FAKEACCT/SavedVariables/Foo.lua",
            "Account/FAKEACCT/config-cache.wtf",
            "Account/FAKEACCT/FakeRealm/FakeChar/SavedVariables/Foo.lua",
            "WTF/Account/OTHER/Realm/",
            "Config.wtf",
        ];
        let r = WtfPathRedactor::for_paths(entries.into_iter());
        assert_eq!(
            format!("{r:?}"),
            "WtfPathRedactor { accounts: 2, realms: 2, characters: 1, truncated: false }"
        );
        assert_eq!(
            r.rel(entries[2]),
            "Account/account 1/realm 1/character 1/SavedVariables/Foo.lua"
        );
        assert_eq!(
            r.rel("WTF/Account/OTHER/Realm/"),
            "WTF/Account/account 2/realm 2/"
        );
        assert_eq!(r.rel(entries[1]), "Account/account 1/config-cache.wtf");
    }

    #[test]
    fn debug_prints_counts_only() {
        let (_dir, c) = client(&["FAKEACCT/FakeRealm/FakeChar", "123456#1"]);
        let r = WtfPathRedactor::for_client(&c);
        let shown = format!("{r:?} {r:#?}");
        for name in [
            "FAKEACCT",
            "FakeRealm",
            "FakeChar",
            "fakeacct",
            "fakerealm",
            "fakechar",
            "123456",
        ] {
            assert!(!shown.contains(name), "{shown}");
        }
        assert!(shown.contains("accounts: 2"), "{shown}");
    }

    #[test]
    fn a_client_without_wtf_learns_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let r = WtfPathRedactor::for_client(&ClientPaths::new(dir.path()));
        assert_eq!(
            r.rel(FOO),
            "Account/account ?/realm ?/character ?/SavedVariables/Foo.lua"
        );
        assert_eq!(r.text("FakeChar"), "FakeChar");
    }
}
