//! The TOC parser and `loaded_toc`, which applies the client's load rules
//! case-insensitively.
//!
//! [`parse_toc`] turns the bytes of a `<Folder>/<Folder>[_Suffix].toc` into a [`Toc`]
//! (vision §4.2, §4.3). It runs on archives from the internet, so it is infallible and
//! never panics: anything odd becomes a [`TocIssue`]. Like the client, it reads only the
//! first [`TOC_LINE_MAX`] bytes of each line. `## Key: Value` lines are directives,
//! other `#` lines comments, and every other line a file reference. The parser
//! evaluates no conditionals and does not depend on the client's rules; [`loaded_toc`]
//! and [`verdict`] apply them.

mod loaded;

use std::collections::BTreeMap;

pub use loaded::{
    DiskFolder, FolderView, LoadedToc, LoadedTocSummary, TocLevel, Verdict, VerdictReport,
    WrongFlavorReason, effective_interfaces, judge, loaded_toc, read_bounded, verdict,
    verify_target,
};

use schemars::JsonSchema;
use serde::Serialize;

use crate::limits::TOC_LINE_MAX;
use crate::model::FolderName;

/// A parsed TOC.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Toc {
    /// Keyed by the lowercased key; the first occurrence of a key wins.
    pub directives: BTreeMap<String, Directive>,
    /// The `## Interface` value as written (never `Interface-Vanilla` and the like).
    pub interface_raw: Option<String>,
    /// Every number in the comma list (clients since 10.2.7).
    pub interfaces: Vec<u32>,
    /// The leading digits, which a client without comma lists reads (unverified).
    pub interface_leading: Option<u32>,
    pub title: Option<String>,
    /// Kept verbatim, `@project-version@` tokens included.
    pub version: Option<String>,
    pub author: Option<String>,
    pub notes: Option<String>,
    /// `RequiredDeps`, `Dependencies` and every `Dep*` key, in file order, without
    /// case-insensitive duplicates.
    pub dependencies: Vec<String>,
    pub optional_deps: Vec<String>,
    pub load_with: Vec<String>,
    pub load_managers: Vec<String>,
    /// `## LoadOnDemand: 1`.
    pub load_on_demand: bool,
    /// Lowercased.
    pub allow_load_game_type: Vec<String>,
    /// Lowercased.
    pub exclude_load_game_type: Vec<String>,
    pub saved_variables: Vec<String>,
    pub saved_variables_per_character: Vec<String>,
    /// Every `X-` directive, by its original spelling.
    pub x_fields: BTreeMap<String, String>,
    pub files: Vec<TocFile>,
    pub issues: Vec<TocIssue>,
}

/// One `## Key: Value` line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Directive {
    /// As written.
    pub key: String,
    pub value: String,
    /// Bracketed text between the key and the colon, kept raw and never evaluated
    /// (directive conditionals, 12.0.7+; syntax unverified).
    pub condition: Option<String>,
    /// 1-based.
    pub line: u32,
}

/// A file reference line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct TocFile {
    /// The trimmed line.
    pub raw: String,
    /// With `\` normalized to `/`.
    pub path: String,
    /// A trailing `[...]` block after whitespace, without the brackets, e.g.
    /// `AllowLoadGameType standard`.
    pub condition: Option<String>,
    /// `Family`, `Game` or `TextLocale` for each such token in the path.
    pub path_vars: Vec<String>,
}

/// Something odd in a TOC; parsing carries on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TocIssue {
    /// The line was longer than the client reads; the rest was ignored.
    LineTruncated { line: u32 },
    /// A later occurrence of a key the TOC already set.
    DuplicateDirective { key: String, line: u32 },
    /// An `## Interface` entry that is not a number.
    BadInterface { value: String },
    /// A dependency that cannot be a folder name.
    BadDependency { name: String },
}

const PATH_VARS: &[&str] = &["Family", "Game", "TextLocale"];

/// Parses a TOC. Infallible: problems are recorded in [`Toc::issues`].
pub fn parse_toc(bytes: &[u8]) -> Toc {
    let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    let mut toc = Toc::default();
    for (index, raw) in lines(bytes).enumerate() {
        let line = u32::try_from(index + 1).unwrap_or(u32::MAX);
        let raw = match raw.get(..TOC_LINE_MAX) {
            Some(head) if raw.len() > TOC_LINE_MAX => {
                toc.issues.push(TocIssue::LineTruncated { line });
                head
            }
            _ => raw,
        };
        let text = String::from_utf8_lossy(raw);
        let trimmed = text.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("##") {
            if let Some(directive) = directive(rest, line) {
                match toc.directives.entry(directive.key.to_ascii_lowercase()) {
                    std::collections::btree_map::Entry::Occupied(_) => {
                        toc.issues.push(TocIssue::DuplicateDirective {
                            key: directive.key,
                            line,
                        });
                    }
                    std::collections::btree_map::Entry::Vacant(slot) => {
                        slot.insert(directive);
                    }
                }
            }
            continue;
        }
        if trimmed.starts_with('#') {
            continue;
        }
        toc.files.push(file(trimmed));
    }
    derive_fields(&mut toc);
    toc
}

/// Lines split on `\n`, `\r\n` or a lone `\r`.
fn lines(bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut rest = bytes;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let end = rest
            .iter()
            .position(|b| *b == b'\n' || *b == b'\r')
            .unwrap_or(rest.len());
        let (line, tail) = rest.split_at(end);
        rest = match tail {
            [b'\r', b'\n', more @ ..] | [b'\r' | b'\n', more @ ..] => more,
            _ => &[],
        };
        Some(line)
    })
}

/// `Key [condition]: value` after the `##`; `None` without a colon outside brackets
/// or with an empty key.
fn directive(rest: &str, line: u32) -> Option<Directive> {
    let mut depth = 0usize;
    let colon = rest.char_indices().find_map(|(i, c)| {
        match c {
            '[' => depth += 1,
            ']' => depth = depth.saturating_sub(1),
            ':' if depth == 0 => return Some(i),
            _ => {}
        }
        None
    })?;
    let (head, value) = (rest.get(..colon)?, rest.get(colon + 1..)?);
    let (key, condition) = match head.find('[') {
        Some(open) => (head.get(..open)?, Some(head.get(open..)?.trim().to_owned())),
        None => (head, None),
    };
    let key = key.trim();
    if key.is_empty() {
        return None;
    }
    Some(Directive {
        key: key.to_owned(),
        value: value.trim().to_owned(),
        condition,
        line,
    })
}

fn file(trimmed: &str) -> TocFile {
    let (path, condition) = match trimmed
        .strip_suffix(']')
        .and_then(|t| t.rfind('[').map(|i| (t, i)))
    {
        Some((body, open))
            if body
                .get(..open)
                .is_some_and(|p| p.ends_with(char::is_whitespace)) =>
        {
            (
                body.get(..open).unwrap_or_default().trim_end(),
                body.get(open + 1..).map(|c| c.trim().to_owned()),
            )
        }
        _ => (trimmed, None),
    };
    let mut path_vars = Vec::new();
    for var in PATH_VARS {
        if path.contains(&format!("[{var}]")) && !path_vars.iter().any(|v: &String| v == var) {
            path_vars.push((*var).to_owned());
        }
    }
    path_vars.sort_by_key(|v| path.find(&format!("[{v}]")));
    TocFile {
        raw: trimmed.to_owned(),
        path: path.replace('\\', "/"),
        condition,
        path_vars,
    }
}

/// Fills the typed fields from the directives.
fn derive_fields(toc: &mut Toc) {
    let value = |key: &str| toc.directives.get(key).map(|d| d.value.clone());
    toc.interface_raw = value("interface");
    toc.title = value("title");
    toc.version = value("version");
    toc.author = value("author");
    toc.notes = value("notes");
    toc.optional_deps = list(value("optionaldeps"));
    toc.load_with = list(value("loadwith"));
    toc.load_managers = list(value("loadmanagers"));
    toc.load_on_demand = value("loadondemand").is_some_and(|v| v == "1");
    toc.allow_load_game_type = lowercase(list(value("allowloadgametype")));
    toc.exclude_load_game_type = lowercase(list(value("excludeloadgametype")));
    toc.saved_variables = list(value("savedvariables"));
    toc.saved_variables_per_character = list(value("savedvariablespercharacter"));
    toc.x_fields = toc
        .directives
        .iter()
        .filter(|(lower, _)| lower.starts_with("x-"))
        .map(|(_, d)| (d.key.clone(), d.value.clone()))
        .collect();

    if let Some(raw) = &toc.interface_raw {
        for entry in raw.split(',').map(str::trim).filter(|e| !e.is_empty()) {
            match entry.parse::<u32>() {
                Ok(n) => toc.interfaces.push(n),
                Err(_) => toc.issues.push(TocIssue::BadInterface {
                    value: entry.to_owned(),
                }),
            }
        }
        let digits: String = raw
            .trim()
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        toc.interface_leading = digits.parse().ok();
    }

    let mut deps: Vec<&Directive> = toc
        .directives
        .iter()
        .filter(|(lower, _)| *lower == "requireddeps" || lower.starts_with("dep"))
        .map(|(_, d)| d)
        .collect();
    deps.sort_by_key(|d| d.line);
    let mut dependencies: Vec<String> = Vec::new();
    for name in deps.iter().flat_map(|d| list(Some(d.value.clone()))) {
        if !dependencies
            .iter()
            .any(|seen| seen.eq_ignore_ascii_case(&name))
        {
            dependencies.push(name);
        }
    }
    for name in &dependencies {
        if FolderName::new(name).is_err() {
            toc.issues
                .push(TocIssue::BadDependency { name: name.clone() });
        }
    }
    toc.dependencies = dependencies;
}

/// A comma list, trimmed, without empty entries.
fn list(value: Option<String>) -> Vec<String> {
    value
        .map(|v| {
            v.split(',')
                .map(str::trim)
                .filter(|e| !e.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn lowercase(items: Vec<String>) -> Vec<String> {
    items.into_iter().map(|s| s.to_ascii_lowercase()).collect()
}

impl Toc {
    /// A directive's value, by key ignoring ASCII case.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.directives
            .get(&key.to_ascii_lowercase())
            .map(|d| d.value.as_str())
    }

    /// The dependencies that are valid folder names (others are in [`Toc::issues`]).
    pub fn dependency_folders(&self) -> Vec<FolderName> {
        self.dependencies
            .iter()
            .filter_map(|name| FolderName::new(name).ok())
            .collect()
    }

    fn x(&self, key: &str) -> Option<&str> {
        self.x_fields
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| v.as_str())
    }

    pub fn curse_project_id(&self) -> Option<&str> {
        self.x("X-Curse-Project-ID")
    }

    pub fn wowi_id(&self) -> Option<&str> {
        self.x("X-WoWI-ID")
    }

    pub fn wago_id(&self) -> Option<&str> {
        self.x("X-Wago-ID")
    }

    pub fn tukui_project_id(&self) -> Option<&str> {
        self.x("X-Tukui-ProjectID")
    }

    /// `X-Tukui-ProjectFolders` as a list.
    pub fn tukui_project_folders(&self) -> Vec<String> {
        list(self.x("X-Tukui-ProjectFolders").map(str::to_owned))
    }

    pub fn website(&self) -> Option<&str> {
        self.x("X-Website")
    }

    pub fn addon_provider(&self) -> Option<&str> {
        self.x("X-AddonProvider")
    }

    pub fn part_of(&self) -> Option<&str> {
        self.x("X-Part-Of")
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn parse(text: &str) -> Toc {
        parse_toc(text.as_bytes())
    }

    #[test]
    fn interfaces() {
        let toc = parse("## Interface: 120105, 50504, 11509\n## Interface-Vanilla: 11509\n");
        assert_eq!(toc.interfaces, [120_105, 50_504, 11_509]);
        assert_eq!(toc.interface_leading, Some(120_105));
        assert_eq!(toc.interface_raw.as_deref(), Some("120105, 50504, 11509"));
        assert_eq!(
            toc.get("interface-vanilla"),
            Some("11509"),
            "kept as a directive only"
        );

        let toc = parse("## interface: 30300\n");
        assert_eq!(
            (toc.interfaces.as_slice(), toc.interface_leading),
            (&[30_300][..], Some(30_300))
        );
        let toc = parse("## Interface: 1x2, , 11509\n");
        assert_eq!(toc.interfaces, [11_509]);
        assert_eq!(toc.interface_leading, Some(1));
        assert_eq!(
            toc.issues,
            [TocIssue::BadInterface {
                value: "1x2".into()
            }]
        );
        let toc = parse("## Interface:\n");
        assert_eq!((toc.interfaces.len(), toc.interface_leading), (0, None));
        assert_eq!(parse("## Title: x\n").interface_raw, None);
    }

    #[test]
    fn dependencies_merge_in_file_order() {
        let toc =
            parse("## RequiredDeps: A\n## Dependencies: B, a\n## DepS: C\n## OptionalDeps: D, E\n");
        assert_eq!(toc.dependencies, ["A", "B", "C"]);
        assert_eq!(toc.optional_deps, ["D", "E"]);
        assert_eq!(
            toc.dependency_folders()
                .iter()
                .map(FolderName::as_str)
                .collect::<Vec<_>>(),
            ["A", "B", "C"]
        );
        let toc = parse("## Dependencies: Good, ../evil, .hidden\n");
        assert_eq!(toc.dependency_folders().len(), 1);
        assert_eq!(
            toc.issues,
            [
                TocIssue::BadDependency {
                    name: "../evil".into()
                },
                TocIssue::BadDependency {
                    name: ".hidden".into()
                },
            ]
        );
    }

    #[test]
    fn load_directives() {
        let toc = parse(
            "## LoadOnDemand: 1\n## LoadWith: Blizzard_AuctionUI\n## LoadManagers: AddonLoader\n\
             ## AllowLoadGameType: Standard, PLUNDERSTORM\n## ExcludeLoadGameType: wowhack\n\
             ## SavedVariables: A, B\n## SavedVariablesPerCharacter: C\n",
        );
        assert!(toc.load_on_demand);
        assert_eq!(toc.load_with, ["Blizzard_AuctionUI"]);
        assert_eq!(toc.load_managers, ["AddonLoader"]);
        assert_eq!(toc.allow_load_game_type, ["standard", "plunderstorm"]);
        assert_eq!(toc.exclude_load_game_type, ["wowhack"]);
        assert_eq!(
            (
                toc.saved_variables.as_slice(),
                toc.saved_variables_per_character.as_slice()
            ),
            (&["A".to_owned(), "B".to_owned()][..], &["C".to_owned()][..])
        );
        for (value, expected) in [("0", false), ("yes", false), (" 1 ", true), ("", false)] {
            assert_eq!(
                parse(&format!("## LoadOnDemand: {value}\n")).load_on_demand,
                expected,
                "{value:?}"
            );
        }
    }

    #[test]
    fn directive_lines() {
        let toc = parse(
            "## Title: First\n## TITLE: Second\n## no colon here\n# comment: not a directive\n##: empty key\n",
        );
        assert_eq!(toc.title.as_deref(), Some("First"));
        assert_eq!(
            toc.issues,
            [TocIssue::DuplicateDirective {
                key: "TITLE".into(),
                line: 2
            }]
        );
        assert_eq!(toc.directives.len(), 1);
        assert!(toc.files.is_empty());

        let toc = parse(
            "## Title [AllowLoadGameType standard]: Conditional\n## Notes [Game:Mainline]: colon inside\n",
        );
        let title = &toc.directives["title"];
        assert_eq!(
            (title.value.as_str(), title.condition.as_deref()),
            ("Conditional", Some("[AllowLoadGameType standard]"))
        );
        let notes = &toc.directives["notes"];
        assert_eq!(
            (notes.key.as_str(), notes.value.as_str()),
            ("Notes", "colon inside")
        );
        assert_eq!(notes.condition.as_deref(), Some("[Game:Mainline]"));
        let url = parse("## X-Website: https://example.org/a:b\n");
        assert_eq!(url.website(), Some("https://example.org/a:b"));
    }

    #[test]
    fn file_lines() {
        let toc = parse(
            "libs\\LibStub\\LibStub.lua\nLocales\\[TextLocale].lua\nRetail.lua [AllowLoadGameType standard]\nCore-[Family]-[Game].lua\n  Spaced.lua  \n",
        );
        let files: Vec<(&str, Option<&str>, Vec<&str>)> = toc
            .files
            .iter()
            .map(|f| {
                (
                    f.path.as_str(),
                    f.condition.as_deref(),
                    f.path_vars.iter().map(String::as_str).collect(),
                )
            })
            .collect();
        assert_eq!(
            files,
            [
                ("libs/LibStub/LibStub.lua", None, vec![]),
                ("Locales/[TextLocale].lua", None, vec!["TextLocale"]),
                ("Retail.lua", Some("AllowLoadGameType standard"), vec![]),
                ("Core-[Family]-[Game].lua", None, vec!["Family", "Game"]),
                ("Spaced.lua", None, vec![]),
            ]
        );
        assert_eq!(toc.files[0].raw, "libs\\LibStub\\LibStub.lua");
    }

    #[test]
    fn line_endings_and_bom() {
        let plain = parse("## Interface: 120105\n## Title: T\nCore.lua\n");
        assert_eq!(
            parse("\u{feff}## Interface: 120105\r\n## Title: T\r\nCore.lua\r\n"),
            plain
        );
        assert_eq!(parse("## Interface: 120105\r## Title: T\rCore.lua"), plain);
        assert_eq!(
            parse("\n\n## Interface: 120105\n\n## Title: T\nCore.lua").title,
            plain.title
        );
    }

    #[test]
    fn truncation_counts_bytes() {
        for (len, truncated) in [(1024, false), (1025, true), (2000, true)] {
            let value = "x".repeat(len - "## Title: ".len());
            let toc = parse(&format!("## Title: {value}\n## Version: 1\n"));
            assert_eq!(
                toc.title.as_ref().map(String::len),
                Some(1024 - "## Title: ".len()),
                "{len}"
            );
            assert_eq!(
                toc.issues.contains(&TocIssue::LineTruncated { line: 1 }),
                truncated,
                "{len}"
            );
            assert_eq!(toc.version.as_deref(), Some("1"));
        }
        // A multibyte character cut by the byte limit decodes lossily.
        let toc = parse(&format!(
            "## Title: {}é\n",
            "x".repeat(1024 - "## Title: ".len() - 1)
        ));
        assert!(toc.title.unwrap().ends_with('\u{fffd}'));
    }

    #[test]
    fn x_fields() {
        let toc = parse_toc(include_bytes!("../tests/fixtures/toc/parser/modern.toc"));
        assert_eq!(
            (toc.curse_project_id(), toc.wowi_id(), toc.wago_id()),
            (Some("123456"), Some("7890"), Some("abcDEF12"))
        );
        assert_eq!(toc.website(), Some("https://example.org/demo"));
        let toc = parse_toc(include_bytes!(
            "../tests/fixtures/toc/parser/unreplaced.toc"
        ));
        assert_eq!(
            (toc.tukui_project_id(), toc.addon_provider(), toc.part_of()),
            (Some("42"), Some("wago"), Some("DemoSuite"))
        );
        assert_eq!(toc.tukui_project_folders(), ["Packaged", "PackagedOptions"]);
        assert_eq!(toc.version.as_deref(), Some("@project-version@"));
        assert_eq!(
            toc.x_fields.keys().next().map(String::as_str),
            Some("X-AddonProvider")
        );
    }

    #[test]
    fn fixtures() {
        let modern = parse_toc(include_bytes!("../tests/fixtures/toc/parser/modern.toc"));
        assert_eq!(modern.interfaces, [120_105, 50_504, 11_509], "{modern:#?}");
        assert_eq!(modern.files.len(), 4);
        assert_eq!(
            modern.files[3].condition.as_deref(),
            Some("AllowLoadGameType standard")
        );
        assert!(modern.issues.is_empty(), "{modern:#?}");
        let wotlk = parse_toc(include_bytes!("../tests/fixtures/toc/parser/wotlk.toc"));
        assert_eq!(
            (wotlk.interface_leading, wotlk.load_on_demand),
            (Some(30_300), true)
        );
        assert_eq!(wotlk.dependencies, ["Blizzard_CombatLog"]);
        assert_eq!(wotlk.files[0].path, "LegacyDemo.xml");
    }

    fn render(directives: &[(String, String)], files: &[String]) -> String {
        let mut text = String::new();
        for (key, value) in directives {
            text.push_str(&format!("## {key}: {value}\n"));
        }
        for file in files {
            text.push_str(file);
            text.push('\n');
        }
        text
    }

    proptest! {
        #[test]
        fn arbitrary_bytes_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..4096)) {
            let _ = parse_toc(&bytes);
        }

        #[test]
        fn hostile_shapes_never_panic(
            parts in proptest::collection::vec(prop_oneof![
                Just("##".to_owned()), Just(":".to_owned()), Just("[".to_owned()), Just("]".to_owned()),
                Just("\r".to_owned()), Just("\n".to_owned()), Just("\0".to_owned()), Just("é".to_owned()),
                Just(",".to_owned()), "[a-zA-Z0-9 ]{0,8}",
            ], 0..64),
            long in 0usize..(100 * 1024),
        ) {
            let mut text = parts.concat();
            text.push_str(&"y".repeat(long));
            let toc = parse_toc(text.as_bytes());
            for directive in toc.directives.values() {
                prop_assert!(directive.key.len() <= TOC_LINE_MAX);
            }
        }

        #[test]
        fn rendered_tocs_parse_back(
            keys in proptest::collection::btree_set("[a-z][a-z0-9-]{0,10}", 0..8),
            values in proptest::collection::vec("[A-Za-z0-9][A-Za-z0-9 ,._-]{0,15}[A-Za-z0-9]", 8),
            files in proptest::collection::vec("[A-Za-z0-9_][A-Za-z0-9_/.-]{0,20}", 0..6),
        ) {
            let directives: Vec<(String, String)> = keys.into_iter().zip(values).collect();
            let text = render(&directives, &files);
            let toc = parse(&text);
            prop_assert_eq!(toc.directives.len(), directives.len(), "{}", text);
            for (key, value) in &directives {
                prop_assert_eq!(toc.get(key), Some(value.as_str()), "{}", text);
            }
            let paths: Vec<&str> = toc.files.iter().map(|f| f.path.as_str()).collect();
            prop_assert_eq!(paths, files.iter().map(String::as_str).collect::<Vec<_>>());
        }
    }
}
