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
    WrongFlavorReason, best_in_family, effective_interfaces, judge, loaded_toc, primary_interface,
    read_bounded, verdict, verify_target,
};

use schemars::JsonSchema;
use serde::Serialize;

use crate::limits::TOC_LINE_MAX;
use crate::model::FolderName;
use crate::rules::ClientRules;

/// A parsed TOC.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Toc {
    /// The unconditional directives, keyed by the lowercased key; the first occurrence
    /// of a key wins.
    pub directives: BTreeMap<String, Directive>,
    /// Every directive with a condition (`## Key [AllowLoadGameType x]: Value`, 12.0.7+),
    /// in line order: they never displace an unconditional one nor count as duplicates.
    /// [`Toc::dependencies_for`] and [`Toc::title_for`] evaluate them for a client.
    pub conditional: Vec<Directive>,
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
    /// The unconditional `RequiredDeps`, `Dependencies` and every `Dep*` key, in file
    /// order, without case-insensitive duplicates. A caller that knows the client uses
    /// [`Toc::dependencies_for`], which adds the conditional variants that apply.
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
    /// Bracketed text between the key and the colon, kept raw for display (directive
    /// conditionals, 12.0.7+).
    pub condition: Option<String>,
    /// The condition, parsed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parsed_condition: Option<Condition>,
    /// 1-based.
    pub line: u32,
}

/// A directive's condition: `[AllowLoadGameType mainline, vanilla]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Condition {
    pub kind: ConditionKind,
    /// Lowercased; split on commas and whitespace.
    pub values: Vec<String>,
}

/// What a condition tests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConditionKind {
    /// The client is one of the values.
    AllowLoadGameType,
    /// The client is none of the values.
    ExcludeLoadGameType,
    /// Anything else, raw (an unknown keyword, `[Game:Mainline]`, an empty bracket):
    /// never evaluated as true or false.
    Other(String),
}

impl Condition {
    /// The bracketed text of a directive (`[AllowLoadGameType mainline]`), parsed. The
    /// first whitespace-separated token, ignoring ASCII case, is the kind; the rest are
    /// the values. A known kind without values is `Other`.
    pub fn parse(raw: &str) -> Self {
        let other = || Self {
            kind: ConditionKind::Other(raw.to_owned()),
            values: Vec::new(),
        };
        let inner = raw.trim();
        let inner = inner.strip_prefix('[').unwrap_or(inner);
        let inner = inner.strip_suffix(']').unwrap_or(inner).trim();
        let (keyword, rest) = inner.split_once(char::is_whitespace).unwrap_or((inner, ""));
        let kind = if keyword.eq_ignore_ascii_case("AllowLoadGameType") {
            ConditionKind::AllowLoadGameType
        } else if keyword.eq_ignore_ascii_case("ExcludeLoadGameType") {
            ConditionKind::ExcludeLoadGameType
        } else {
            return other();
        };
        let values: Vec<String> = rest
            .split(|c: char| c == ',' || c.is_whitespace())
            .filter(|v| !v.is_empty())
            .map(str::to_ascii_lowercase)
            .collect();
        if values.is_empty() {
            return other();
        }
        Self { kind, values }
    }

    /// Whether the condition holds on a client named `names` (its game types and
    /// family-level names, lowercased: [`loaded::client_names`]); `None` for a kind
    /// this build does not know.
    pub fn admits(&self, names: &[String]) -> Option<bool> {
        let named = self.values.iter().any(|v| names.contains(v));
        match self.kind {
            ConditionKind::AllowLoadGameType => Some(named),
            ConditionKind::ExcludeLoadGameType => Some(!named),
            ConditionKind::Other(_) => None,
        }
    }
}

/// A folder's dependencies on one client.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, JsonSchema)]
pub struct DepList {
    /// Unconditional, plus the conditional variants that admit the client, in line
    /// order.
    pub required: Vec<FolderName>,
    /// `OptionalDeps`, the same way: never installed.
    pub optional: Vec<FolderName>,
    /// Required under a condition of a kind this build does not know: reported, never
    /// installed.
    pub unknown_condition: Vec<FolderName>,
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
                if directive.condition.is_some() {
                    toc.conditional.push(directive);
                    continue;
                }
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
        parsed_condition: condition.as_deref().map(Condition::parse),
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

/// A title without its UI escapes: colour codes (`|cAARRGGBB` … `|r`), textures
/// (`|T…|t`) and atlases (`|A…|a`) removed, `||` read as `|`, whitespace runs collapsed
/// and trimmed. Infallible: an escape that is malformed or never closed stays as text.
/// `plain_title("|cff33ff99Details!|r Damage Meter")` is `Details! Damage Meter`.
pub fn plain_title(raw: &str) -> String {
    let chars: Vec<char> = raw.chars().collect();
    let mut out = String::with_capacity(raw.len());
    let mut i = 0;
    while let Some(&c) = chars.get(i) {
        if c == '|' {
            let next = chars.get(i + 1).copied();
            // The end of a `|T…|t` or `|A…|a` escape, past its closing pair.
            let closing = |close: char| {
                (i + 2..chars.len().saturating_sub(1))
                    .find(|&j| chars.get(j) == Some(&'|') && chars.get(j + 1) == Some(&close))
                    .map(|j| j + 2)
            };
            let skip = match next {
                Some('c' | 'C')
                    if chars
                        .get(i + 2..i + 10)
                        .is_some_and(|hex| hex.iter().all(char::is_ascii_hexdigit)) =>
                {
                    Some(i + 10)
                }
                Some('r' | 'R') => Some(i + 2),
                Some('T') => closing('t'),
                Some('A') => closing('a'),
                Some('|') => {
                    out.push('|');
                    Some(i + 2)
                }
                _ => None,
            };
            if let Some(end) = skip {
                i = end;
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
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

    /// The unconditional dependencies that are valid folder names (others are in
    /// [`Toc::issues`]).
    pub fn dependency_folders(&self) -> Vec<FolderName> {
        self.dependencies
            .iter()
            .filter_map(|name| FolderName::new(name).ok())
            .collect()
    }

    /// The dependencies on the client of `rules`: required (`RequiredDeps`,
    /// `Dependencies`, `Dep*`) and optional (`OptionalDeps`), unconditional lines and
    /// the conditional variants whose condition admits the client, in line order and
    /// without case-insensitive duplicates; invalid names left out. A required name
    /// under a condition of unknown kind goes to `unknown_condition` (an optional one
    /// is dropped: never installed anyway).
    ///
    /// Several variants of one directive are read as the union of those that apply,
    /// as inline file conditionals behave. UNVERIFIED against the client. A client
    /// without directive conditions (`rules.directive_conditionals` false: before 12.0.7,
    /// and every legacy client) reads such a line's key as another key, so no
    /// conditional variant applies there.
    pub fn dependencies_for(&self, rules: &ClientRules) -> DepList {
        let names = loaded::client_names(rules);
        let conditional: &[Directive] = if rules.directive_conditionals {
            &self.conditional
        } else {
            &[]
        };
        let mut lines: Vec<&Directive> = self.directives.values().chain(conditional).collect();
        lines.sort_by_key(|d| d.line);
        let mut deps = DepList::default();
        let push = |into: &mut Vec<FolderName>, value: &str| {
            for name in list(Some(value.to_owned())) {
                if let Ok(name) = FolderName::new(&name)
                    && !into.contains(&name)
                {
                    into.push(name);
                }
            }
        };
        for d in lines {
            let key = d.key.to_ascii_lowercase();
            let required = key == "requireddeps" || key.starts_with("dep");
            if !required && key != "optionaldeps" {
                continue;
            }
            let admits = d.parsed_condition.as_ref().map(|c| c.admits(&names));
            match (admits, required) {
                (None | Some(Some(true)), true) => push(&mut deps.required, &d.value),
                (None | Some(Some(true)), false) => push(&mut deps.optional, &d.value),
                (Some(None), true) => push(&mut deps.unknown_condition, &d.value),
                (Some(None), false) | (Some(Some(false)), _) => {}
            }
        }
        deps.unknown_condition
            .retain(|name| !deps.required.contains(name));
        deps
    }

    /// The title the client of `rules` shows: the first conditional `Title` variant
    /// that admits it (on a client with directive conditions), else the unconditional
    /// one. Which one the client shows when both exist is UNVERIFIED.
    pub fn title_for(&self, rules: &ClientRules) -> Option<&str> {
        let names = loaded::client_names(rules);
        self.conditional
            .iter()
            .filter(|d| rules.directive_conditionals && d.key.eq_ignore_ascii_case("title"))
            .find(|d| {
                d.parsed_condition
                    .as_ref()
                    .and_then(|c| c.admits(&names))
                    .unwrap_or(false)
            })
            .map(|d| d.value.as_str())
            .or(self.title.as_deref())
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
        assert!(toc.directives.is_empty(), "conditional lines live apart");
        let [title, notes] = toc.conditional.as_slice() else {
            unreachable!("{:?}", toc.conditional)
        };
        assert_eq!(
            (title.value.as_str(), title.condition.as_deref()),
            ("Conditional", Some("[AllowLoadGameType standard]"))
        );
        assert_eq!(
            title.parsed_condition,
            Some(Condition {
                kind: ConditionKind::AllowLoadGameType,
                values: vec!["standard".into()]
            })
        );
        assert_eq!(
            (notes.key.as_str(), notes.value.as_str()),
            ("Notes", "colon inside")
        );
        assert_eq!(notes.condition.as_deref(), Some("[Game:Mainline]"));
        assert_eq!(
            notes.parsed_condition.as_ref().map(|c| &c.kind),
            Some(&ConditionKind::Other("[Game:Mainline]".into()))
        );
        assert_eq!(toc.title, None, "only a conditional title");

        // Conditional variants never count as duplicates; an unconditional one does.
        let toc = parse(
            "## Title [AllowLoadGameType standard]: A\n## Title [AllowLoadGameType vanilla]: B\n## Title: C\n## Title: D\n",
        );
        assert_eq!(toc.conditional.len(), 2);
        assert_eq!(toc.title.as_deref(), Some("C"));
        assert_eq!(
            toc.issues,
            [TocIssue::DuplicateDirective {
                key: "Title".into(),
                line: 4
            }]
        );
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

    #[test]
    fn conditions_parse_by_kind_and_values() {
        let kinds = |raw: &str| {
            let c = Condition::parse(raw);
            (c.kind, c.values)
        };
        assert_eq!(
            kinds("[AllowLoadGameType standard]"),
            (
                ConditionKind::AllowLoadGameType,
                vec!["standard".to_owned()]
            )
        );
        assert_eq!(
            kinds("[allowloadgametype  Mainline, Vanilla tbc]"),
            (
                ConditionKind::AllowLoadGameType,
                ["mainline", "vanilla", "tbc"].map(str::to_owned).to_vec()
            )
        );
        assert_eq!(
            kinds("[ExcludeLoadGameType camelot]").0,
            ConditionKind::ExcludeLoadGameType
        );
        for raw in [
            "[SomeFutureCondition x]",
            "[Game:Mainline]",
            "[]",
            "[AllowLoadGameType]",
        ] {
            assert_eq!(
                kinds(raw),
                (ConditionKind::Other(raw.to_owned()), Vec::new()),
                "{raw}"
            );
        }
    }

    /// The builtin rules of a Battle.net (or `custom`) client of `version`.
    fn rules_of(version: &str, custom: bool) -> ClientRules {
        let kind = if custom {
            crate::rules::InstallKind::Custom
        } else {
            crate::rules::InstallKind::Battlenet
        };
        let version: crate::rules::ClientVersion = version.parse().unwrap();
        crate::rules::RulesTable::builtin()
            .rules_for(&version, kind)
            .clone()
    }

    fn names(folders: &[FolderName]) -> Vec<&str> {
        folders.iter().map(FolderName::as_str).collect()
    }

    #[test]
    fn dependencies_are_evaluated_for_each_client() {
        let retail = rules_of("12.1.5.70391", false);
        let era = rules_of("1.15.9.64520", false);
        let forever = rules_of("1.60.1.70245", false);
        let titan = rules_of("3.80.0.65000", false);
        let legacy = rules_of("1.12.1", true);
        assert!(legacy.legacy, "a legacy row");

        let toc = parse("## Dependencies [AllowLoadGameType mainline]: A\n## Dependencies: B\n");
        assert_eq!(names(&toc.dependencies_for(&retail).required), ["A", "B"]);
        assert_eq!(names(&toc.dependencies_for(&era).required), ["B"]);
        assert_eq!(
            names(&toc.dependencies_for(&forever).required),
            ["A", "B"],
            "Forever is named by its family suffix mainline"
        );
        assert_eq!(names(&toc.dependencies_for(&legacy).required), ["B"]);
        assert_eq!(toc.dependencies, ["B"], "the unconditional field alone");

        // The other order keeps both variants, in line order.
        let toc = parse("## Dependencies: B\n## Dependencies [AllowLoadGameType mainline]: A\n");
        assert_eq!(names(&toc.dependencies_for(&retail).required), ["B", "A"]);
        assert!(toc.issues.is_empty(), "{:?}", toc.issues);

        // Exclusion, RequiredDeps and Dep* keys, optional deps, and duplicates.
        let toc = parse(
            "## RequiredDeps [ExcludeLoadGameType vanilla, titan]: Ace3\n\
             ## Dep-Lib: LibStub, ace3\n\
             ## OptionalDeps [AllowLoadGameType vanilla]: Questie\n\
             ## OptionalDeps: Details\n\
             ## LoadWith: Other\n",
        );
        let on = |rules: &ClientRules| {
            let d = toc.dependencies_for(rules);
            (
                names(&d.required)
                    .iter()
                    .map(|s| (*s).to_owned())
                    .collect::<Vec<_>>(),
                names(&d.optional)
                    .iter()
                    .map(|s| (*s).to_owned())
                    .collect::<Vec<_>>(),
            )
        };
        assert_eq!(
            on(&retail),
            (
                vec!["Ace3".into(), "LibStub".into()],
                vec!["Details".into()]
            )
        );
        assert_eq!(
            on(&era),
            (
                vec!["LibStub".into(), "ace3".into()],
                vec!["Questie".into(), "Details".into()]
            )
        );
        assert_eq!(
            on(&titan),
            (
                vec!["LibStub".into(), "ace3".into()],
                vec!["Details".into()]
            )
        );

        // An unknown condition is never required; an optional one is dropped.
        let toc = parse(
            "## Dependencies [SomeFutureCondition x]: Future, B\n## Dependencies: B\n## OptionalDeps [Odd]: Opt\n",
        );
        let deps = toc.dependencies_for(&retail);
        assert_eq!(names(&deps.required), ["B"]);
        assert_eq!(
            names(&deps.unknown_condition),
            ["Future"],
            "B is required anyway"
        );
        assert!(deps.optional.is_empty());
    }

    /// Conditional variants apply only on a client that reads directive conditions:
    /// every current client, none before 12.0.7, and not a row whose override turns the
    /// flag off.
    #[test]
    fn conditional_variants_need_a_client_with_directive_conditionals() {
        use crate::rules::{InstallKind, RulesOverrides, RulesTable};
        let toc = parse(
            "## Dependencies [AllowLoadGameType standard, camelot, vanilla, tbc, wrath, titan, cata, mists]: X\n",
        );
        let required = |rules: &ClientRules| !toc.dependencies_for(rules).required.is_empty();
        for version in [
            "12.1.5", "12.0.7", "1.60.1", "1.15.9", "2.5.6", "3.80.2", "5.5.4",
        ] {
            assert!(required(&rules_of(version, false)), "{version}");
        }
        for version in ["12.0.1", "11.2.0", "3.4.3", "4.4.2"] {
            let rules = rules_of(version, false);
            assert!(!required(&rules), "{version} ({})", rules.name);
        }

        let overrides: RulesOverrides =
            toml::from_str("[client_rules.vanilla]\ndirective_conditionals = false\n").unwrap();
        let table = RulesTable::builtin().with_overrides(&overrides).unwrap();
        let era = table.rules_for(&"1.15.9".parse().unwrap(), InstallKind::Battlenet);
        assert!(!required(era), "turned off by the override");
        let titled = parse("## Title: Plain\n## Title [AllowLoadGameType vanilla]: Era\n");
        assert_eq!(titled.title_for(era), Some("Plain"));
    }

    #[test]
    fn a_title_may_exist_only_as_conditional_variants() {
        let toc = parse(
            "## Title [AllowLoadGameType standard]: |cff1784d1ElvUI|r\n## Title [AllowLoadGameType vanilla]: ElvUI Classic\n",
        );
        assert_eq!(toc.title, None);
        let retail = rules_of("12.1.5.70391", false);
        let era = rules_of("1.15.9.64520", false);
        assert_eq!(toc.title_for(&retail), Some("|cff1784d1ElvUI|r"));
        assert_eq!(toc.title_for(&era), Some("ElvUI Classic"));
        assert_eq!(toc.title_for(&rules_of("1.12.1", true)), None);
        let plain = parse("## Title: Plain\n## Title [AllowLoadGameType vanilla]: Era\n");
        assert_eq!(plain.title_for(&retail), Some("Plain"));
        assert_eq!(
            plain.title_for(&era),
            Some("Era"),
            "a matching variant first"
        );
    }

    #[test]
    fn plain_titles_drop_ui_escapes() {
        for (raw, plain) in [
            ("|cff33ff99Details!|r Damage Meter", "Details! Damage Meter"),
            ("|CFF00FF00Green|R", "Green"),
            ("|TInterface\\Icons\\inv_misc:16:16|t Bags", "Bags"),
            ("|A:raceicon-human-male:16:16|a  Human   UI", "Human UI"),
            ("Pipes || kept", "Pipes | kept"),
            ("|cff12zz Not a colour", "|cff12zz Not a colour"),
            ("Unclosed |Ttexture", "Unclosed |Ttexture"),
            ("Trailing |", "Trailing |"),
            ("|", "|"),
            ("", ""),
            ("  spaced   out  ", "spaced out"),
        ] {
            assert_eq!(plain_title(raw), plain, "{raw:?}");
        }
    }

    proptest! {
        #[test]
        fn plain_titles_never_panic_or_grow(
            parts in proptest::collection::vec(prop_oneof![
                Just("|".to_owned()), Just("|c".to_owned()), Just("|r".to_owned()),
                Just("|T".to_owned()), Just("|t".to_owned()), Just("|A".to_owned()),
                Just("|a".to_owned()), Just("ff00ff00".to_owned()), Just("é".to_owned()),
                "[a-zA-Z0-9 ]{0,6}",
            ], 0..32),
        ) {
            let raw = parts.concat();
            let plain = plain_title(&raw);
            prop_assert!(plain.chars().count() <= raw.chars().count(), "{raw:?} -> {plain:?}");
        }

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
