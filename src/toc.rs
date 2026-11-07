//! The TOC parser and `loaded_toc`, which applies the client's load rules
//! case-insensitively.
//!
//! [`parse_toc`] turns the bytes of a `<Folder>/<Folder>[_Suffix].toc` into a [`Toc`]
//! (vision §4.2, §4.3). It runs on archives from the internet, so it is infallible and
//! never panics: anything odd becomes a [`TocIssue`]. Like the client, it reads only the
//! first [`TOC_LINE_MAX`] bytes of each line. `## Key: Value` lines are directives,
//! other `#` lines comments, and every other line a file reference.
//!
//! A directive line may carry inline directives, `[AllowLoadGameType classic]`,
//! anywhere in it (12.0.7+): they make the line apply on some clients only. For one key
//! the last line that applies wins, except the keys whose lines add up (dependencies,
//! `LoadWith`, `LoadManagers`, saved variables and the game-type lists). [`parse_toc`]
//! reads the lines without inline directives, as no particular client;
//! [`Toc::for_client`] reads the lines a client reads, and [`loaded_toc`] and
//! [`verdict`] judge that reading.

mod loaded;

use std::collections::BTreeMap;

pub use loaded::{
    DiskFolder, FolderView, JUDGE_VERSION, LoadedToc, LoadedTocSummary, TocLevel, Verdict,
    VerdictReport, WrongFlavorReason, best_in_family, effective_interfaces, judge, judge_digest,
    loaded_toc, primary_interface, read_bounded, reason_names_schema, verdict, verify_target,
};

use schemars::JsonSchema;
use serde::Serialize;

use crate::limits::TOC_LINE_MAX;
use crate::model::FolderName;
use crate::rules::ClientRules;

/// A parsed TOC: every directive line, and the fields one reading of them gives (no
/// particular client from [`parse_toc`], one client from [`Toc::for_client`]).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Toc {
    /// Every `## Key: Value` line, in file order.
    pub lines: Vec<Directive>,
    /// The lines read, keyed by the lowercased key: the last one of each key.
    pub directives: BTreeMap<String, Directive>,
    /// The `## Interface` value read (never `Interface-Vanilla` and the like).
    pub interface_raw: Option<String>,
    /// Every number in the comma list (clients since 10.2.7).
    pub interfaces: Vec<u32>,
    /// The leading digits, which a client without comma lists reads (3.3.5a loads
    /// `30300, 30400` as current, observed 2026-10-20).
    pub interface_leading: Option<u32>,
    pub title: Option<String>,
    /// Kept verbatim, `@project-version@` tokens included.
    pub version: Option<String>,
    pub author: Option<String>,
    pub notes: Option<String>,
    /// `RequiredDeps`, `Dependencies` and every `Dep*` key: every line read adds to it,
    /// in file order, without case-insensitive duplicates. A caller that knows the
    /// client uses [`Toc::dependencies_for`].
    pub dependencies: Vec<String>,
    /// The last `OptionalDeps` read.
    pub optional_deps: Vec<String>,
    /// Every line read adds to it, as to the other lists but `optional_deps`.
    pub load_with: Vec<String>,
    pub load_managers: Vec<String>,
    /// `## LoadOnDemand: 1`, the last one read.
    pub load_on_demand: bool,
    /// Lowercased.
    pub allow_load_game_type: Vec<String>,
    /// Lowercased.
    pub exclude_load_game_type: Vec<String>,
    pub saved_variables: Vec<String>,
    pub saved_variables_per_character: Vec<String>,
    /// Every `X-` directive read, by its original spelling.
    pub x_fields: BTreeMap<String, String>,
    pub files: Vec<TocFile>,
    pub issues: Vec<TocIssue>,
}

/// One `## Key: Value` line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Directive {
    /// Without its inline directives, trimmed.
    pub key: String,
    pub value: String,
    /// The line as written, split at its first colon outside brackets: what a client
    /// that reads no inline directives sees.
    pub raw_key: String,
    pub raw_value: String,
    /// Its inline directives (`[AllowLoadGameType classic]`, 12.0.7+), in line order.
    pub conditions: Vec<Condition>,
    /// 1-based.
    pub line: u32,
}

/// An inline directive: `[AllowLoadGameType mainline, vanilla]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Condition {
    pub kind: ConditionKind,
    /// Lowercased; split on commas and whitespace.
    pub values: Vec<String>,
}

/// What an inline directive tests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConditionKind {
    /// The client is one of the values.
    AllowLoadGameType,
    /// The client is none of the values.
    ExcludeLoadGameType,
    /// The environment the addon loads in: `game`, `glue` or `both`.
    AllowLoad,
    /// `global` or `secure`.
    AllowLoadEnvironment,
    /// The client's text locale: unknown offline.
    AllowLoadTextLocale,
    /// Unknown offline.
    Bootstrap,
    /// Where the addon loads, not whether: no condition.
    LoadIntoEnvironment,
    /// A known kind without the values it needs, raw: never evaluated as true or false.
    Other(String),
}

/// The inline directives a `##` line can carry, anywhere in it (warcraft.wiki.gg
/// `TOC_format`, "Inline directives", fetched 2026-10-18; added in 12.0.7). Any other
/// bracket text is part of the line, as a title's `[|cffeda55fCore|r]` is.
const INLINE_DIRECTIVES: &[(&str, ConditionKind)] = &[
    ("AllowLoadGameType", ConditionKind::AllowLoadGameType),
    ("ExcludeLoadGameType", ConditionKind::ExcludeLoadGameType),
    ("AllowLoad", ConditionKind::AllowLoad),
    ("AllowLoadEnvironment", ConditionKind::AllowLoadEnvironment),
    ("AllowLoadTextLocale", ConditionKind::AllowLoadTextLocale),
    ("Bootstrap", ConditionKind::Bootstrap),
    ("LoadIntoEnvironment", ConditionKind::LoadIntoEnvironment),
];

/// The kind of inline directive the bracket text `inner` (without its brackets) is: its
/// first word, ignoring ASCII case.
fn inline_kind(inner: &str) -> Option<ConditionKind> {
    let keyword = inner.split_whitespace().next()?;
    INLINE_DIRECTIVES
        .iter()
        .find(|(name, _)| keyword.eq_ignore_ascii_case(name))
        .map(|(_, kind)| kind.clone())
}

impl Condition {
    /// An inline directive's text (`[AllowLoadGameType mainline]`, brackets optional),
    /// parsed: the first word is the kind, the rest the values. Not an inline
    /// directive, or one without the values it needs, is `Other`.
    pub fn parse(raw: &str) -> Self {
        let other = || Self {
            kind: ConditionKind::Other(raw.to_owned()),
            values: Vec::new(),
        };
        let inner = raw.trim();
        let inner = inner.strip_prefix('[').unwrap_or(inner);
        let inner = inner.strip_suffix(']').unwrap_or(inner).trim();
        let Some(kind) = inline_kind(inner) else {
            return other();
        };
        let rest = inner
            .split_once(char::is_whitespace)
            .map_or("", |(_, rest)| rest);
        let values: Vec<String> = rest
            .split(|c: char| c == ',' || c.is_whitespace())
            .filter(|v| !v.is_empty())
            .map(str::to_ascii_lowercase)
            .collect();
        if values.is_empty() && kind != ConditionKind::Bootstrap {
            return other();
        }
        Self { kind, values }
    }

    /// Whether the line may load on a client named `names` (its game types and
    /// family-level names, lowercased: [`loaded::client_names`]); `None` when that
    /// cannot be known offline.
    ///
    /// Game types are matched strictly: a value must name the client. The client also
    /// lets an `AllowLoadGameType` pass that names no game type it recognizes (the wiki
    /// above), but which names each build recognizes is not known here, so this fails
    /// closed; publishers pair such lines with `ExcludeLoadGameType`. Third-party addons
    /// load in game and in the global environment.
    pub fn admits(&self, names: &[String]) -> Option<bool> {
        let named = self.values.iter().any(|v| names.contains(v));
        let any = |wanted: &[&str]| self.values.iter().any(|v| wanted.contains(&v.as_str()));
        match self.kind {
            ConditionKind::AllowLoadGameType => Some(named),
            ConditionKind::ExcludeLoadGameType => Some(!named),
            ConditionKind::AllowLoad => Some(any(&["game", "both"])),
            ConditionKind::AllowLoadEnvironment => Some(any(&["global"])),
            ConditionKind::LoadIntoEnvironment => Some(true),
            ConditionKind::AllowLoadTextLocale
            | ConditionKind::Bootstrap
            | ConditionKind::Other(_) => None,
        }
    }
}

/// A folder's dependencies on one client.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, JsonSchema)]
pub struct DepList {
    /// Every dependency line the client reads, in line order.
    pub required: Vec<FolderName>,
    /// The last `OptionalDeps` line it reads: never installed.
    pub optional: Vec<FolderName>,
    /// Required on a line whose inline directives cannot be decided offline (a text
    /// locale, say): reported, never installed.
    pub unknown_condition: Vec<FolderName>,
}

/// A file reference line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct TocFile {
    /// The trimmed line.
    pub raw: String,
    /// Without trailing inline directives, `\` normalized to `/`.
    pub path: String,
    /// Its trailing inline directives, in order, without brackets, e.g.
    /// `AllowLoadGameType standard`. Never evaluated: nothing reads a TOC's file list.
    pub conditions: Vec<String>,
    /// `Family`, `Game` or `TextLocale` for each such token in the path.
    pub path_vars: Vec<String>,
}

/// Something odd in a TOC; parsing carries on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TocIssue {
    /// The line was longer than the client reads; the rest was ignored.
    LineTruncated { line: u32 },
    /// A later line sets a key the reading already set, and wins.
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
            toc.lines.extend(directive(rest, line));
            continue;
        }
        if trimmed.starts_with('#') {
            continue;
        }
        toc.files.push(file(trimmed));
    }
    resolve(&mut toc, View::Unconditional);
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

/// The directive after a line's `##`; `None` without a colon outside brackets, or
/// with an empty key once its inline directives are taken out.
fn directive(rest: &str, line: u32) -> Option<Directive> {
    let (raw_key, raw_value) = split_at_colon(rest)?;
    let (text, conditions) = take_inline_directives(rest);
    let (key, value) = split_at_colon(&text)?;
    if key.is_empty() {
        return None;
    }
    Some(Directive {
        key,
        value,
        raw_key,
        raw_value,
        conditions,
        line,
    })
}

/// The key and value of a directive's text, at its first colon outside brackets,
/// trimmed.
fn split_at_colon(text: &str) -> Option<(String, String)> {
    let mut depth = 0usize;
    let colon = text.char_indices().find_map(|(i, c)| {
        match c {
            '[' => depth += 1,
            ']' => depth = depth.saturating_sub(1),
            ':' if depth == 0 => return Some(i),
            _ => {}
        }
        None
    })?;
    let (key, value) = (text.get(..colon)?, text.get(colon + 1..)?);
    Some((key.trim().to_owned(), value.trim().to_owned()))
}

/// `text` without the inline directives in it, and those, parsed in order. A bracket
/// group (the innermost, when brackets nest) is one when its first word names an
/// inline directive; any other stays in the text.
fn take_inline_directives(text: &str) -> (String, Vec<Condition>) {
    let mut kept = String::with_capacity(text.len());
    let mut conditions = Vec::new();
    let mut rest = text;
    while let Some(close) = rest.find(']') {
        let head = rest.get(..close).unwrap_or_default();
        let group = head
            .rfind('[')
            .and_then(|open| Some((head.get(..open)?, head.get(open + 1..)?)))
            .filter(|(_, inner)| inline_kind(inner).is_some());
        match group {
            Some((before, inner)) => {
                kept.push_str(before);
                conditions.push(Condition::parse(inner));
            }
            None => {
                kept.push_str(head);
                kept.push(']');
            }
        }
        rest = rest.get(close + 1..).unwrap_or_default();
    }
    kept.push_str(rest);
    (kept, conditions)
}

fn file(trimmed: &str) -> TocFile {
    let mut path = trimmed;
    let mut conditions = Vec::new();
    // Trailing inline directives, `[A] [B]` or `[A][B]`, last first.
    while let Some(body) = path.strip_suffix(']')
        && let Some(open) = body.rfind('[')
        && let Some(inner) = body.get(open + 1..)
        && inline_kind(inner).is_some()
    {
        conditions.insert(0, inner.trim().to_owned());
        path = body.get(..open).unwrap_or_default().trim_end();
    }
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
        conditions,
        path_vars,
    }
}

/// Which lines a reading takes, and how it reads them.
#[derive(Debug, Clone, Copy)]
enum View<'a> {
    /// No particular client: the lines without inline directives.
    Unconditional,
    /// A client that reads inline directives, named by these game-type names.
    Client(&'a [String]),
    /// A client that reads none (`directive_conditionals` false): every line as
    /// written, its brackets in its key or value.
    Literal,
}

/// How a view reads one line: its key and value, when it applies or cannot be decided
/// offline (`Undecided` lines set no field).
enum Read<'d> {
    Applies(&'d str, &'d str),
    Undecided(&'d str, &'d str),
    Skipped,
}

impl<'a> View<'a> {
    /// The view of the client of `rules`, named by `names` ([`loaded::client_names`]).
    fn of(rules: &ClientRules, names: &'a [String]) -> Self {
        if rules.directive_conditionals {
            Self::Client(names)
        } else {
            Self::Literal
        }
    }

    fn read(self, d: &Directive) -> Read<'_> {
        match self {
            Self::Unconditional if d.conditions.is_empty() => Read::Applies(&d.key, &d.value),
            Self::Unconditional => Read::Skipped,
            Self::Literal => Read::Applies(&d.raw_key, &d.raw_value),
            Self::Client(names) => {
                let mut decided = true;
                for condition in &d.conditions {
                    match condition.admits(names) {
                        Some(true) => {}
                        Some(false) => return Read::Skipped,
                        None => decided = false,
                    }
                }
                if decided {
                    Read::Applies(&d.key, &d.value)
                } else {
                    Read::Undecided(&d.key, &d.value)
                }
            }
        }
    }
}

/// Whether a lowercased key names a dependency: `RequiredDeps`, or a word beginning
/// with `Dep` (`Dependencies`, `Deps`, `Dep-Lib`). A key with brackets in it (an inline
/// directive read literally) is not one word.
fn is_dependency_key(lower: &str) -> bool {
    lower == "requireddeps"
        || (lower.starts_with("dep")
            && lower
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'))
}

/// The list a key's lines add up in (warcraft.wiki.gg `TOC_format`, fetched
/// 2026-10-18: "may be specified multiple times, with later instances appending to the
/// list"); `None` for a key whose last line wins.
fn list_key(lower: &str) -> Option<&'static str> {
    if is_dependency_key(lower) {
        return Some("dependencies");
    }
    [
        "loadwith",
        "loadmanagers",
        "savedvariables",
        "savedvariablespercharacter",
        "allowloadgametype",
        "excludeloadgametype",
    ]
    .into_iter()
    .find(|key| *key == lower)
}

/// Fills the fields from the lines `view` reads: for one key the last line read wins,
/// except the keys whose lines add up ([`list_key`]), without case-insensitive
/// duplicates. A key set again by a later line is a [`TocIssue::DuplicateDirective`].
fn resolve(toc: &mut Toc, view: View<'_>) {
    let mut directives = BTreeMap::new();
    let mut lists: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for d in &toc.lines {
        let Read::Applies(key, value) = view.read(d) else {
            continue;
        };
        let lower = key.to_ascii_lowercase();
        match list_key(&lower) {
            Some(list) => add_up(lists.entry(list).or_default(), value),
            None if directives.contains_key(&lower) => {
                toc.issues.push(TocIssue::DuplicateDirective {
                    key: key.to_owned(),
                    line: d.line,
                });
            }
            None => {}
        }
        let read = Directive {
            key: key.to_owned(),
            value: value.to_owned(),
            ..d.clone()
        };
        directives.insert(lower, read);
    }
    let value = |key: &str| directives.get(key).map(|d: &Directive| d.value.clone());
    let mut listed = |key: &str| lists.remove(key).unwrap_or_default();
    toc.interface_raw = value("interface");
    toc.title = value("title");
    toc.version = value("version");
    toc.author = value("author");
    toc.notes = value("notes");
    toc.optional_deps = list(value("optionaldeps"));
    toc.load_on_demand = value("loadondemand").is_some_and(|v| v == "1");
    toc.dependencies = listed("dependencies");
    toc.load_with = listed("loadwith");
    toc.load_managers = listed("loadmanagers");
    toc.allow_load_game_type = lowercase(listed("allowloadgametype"));
    toc.exclude_load_game_type = lowercase(listed("excludeloadgametype"));
    toc.saved_variables = listed("savedvariables");
    toc.saved_variables_per_character = listed("savedvariablespercharacter");
    toc.x_fields = directives
        .iter()
        .filter(|(lower, _)| lower.starts_with("x-"))
        .map(|(_, d)| (d.key.clone(), d.value.clone()))
        .collect();
    toc.directives = directives;

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

    for name in &toc.dependencies {
        if FolderName::new(name).is_err() {
            toc.issues
                .push(TocIssue::BadDependency { name: name.clone() });
        }
    }
}

/// Adds a comma list's entries to `into`, each once, ignoring ASCII case.
fn add_up(into: &mut Vec<String>, value: &str) {
    for name in list(Some(value.to_owned())) {
        if !into.iter().any(|seen| seen.eq_ignore_ascii_case(&name)) {
            into.push(name);
        }
    }
}

/// A title without its UI escapes: colour codes (`|cAARRGGBB` … `|r`), textures
/// (`|T…|t`) and atlases (`|A…|a`) removed, `||` read as `|`, whitespace runs collapsed
/// and trimmed. Infallible: an escape that is malformed or never closed stays as text.
/// `plain_title("|cff33ff99Details!|r Damage Meter")` is `Details! Damage Meter`.
pub fn plain_title(raw: &str) -> String {
    crate::text::strip_ui_escapes(raw)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
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

    /// This TOC as the client of `rules` reads it: its fields from the lines that apply
    /// there, or on a client without inline directives (`directive_conditionals`
    /// false: before 12.0.7, and every legacy client) from every line as written.
    /// [`loaded_toc`] returns TOCs read this way.
    pub fn for_client(&self, rules: &ClientRules) -> Toc {
        let names = loaded::client_names(rules);
        let mut toc = Toc {
            lines: self.lines.clone(),
            files: self.files.clone(),
            issues: self
                .issues
                .iter()
                .filter(|issue| matches!(issue, TocIssue::LineTruncated { .. }))
                .cloned()
                .collect(),
            ..Toc::default()
        };
        resolve(&mut toc, View::of(rules, &names));
        toc
    }

    /// The dependencies on the client of `rules`: every required line it reads
    /// (`RequiredDeps`, `Dependencies`, `Dep*`) adds up, in line order, and the last
    /// `OptionalDeps` it reads; without case-insensitive duplicates, invalid names left
    /// out. A required name on a line that cannot be decided offline goes to
    /// `unknown_condition` (an optional one is dropped: never installed anyway).
    pub fn dependencies_for(&self, rules: &ClientRules) -> DepList {
        let names = loaded::client_names(rules);
        let view = View::of(rules, &names);
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
        for d in &self.lines {
            let (key, value, decided) = match view.read(d) {
                Read::Applies(key, value) => (key, value, true),
                Read::Undecided(key, value) => (key, value, false),
                Read::Skipped => continue,
            };
            let key = key.to_ascii_lowercase();
            if is_dependency_key(&key) {
                let into = if decided {
                    &mut deps.required
                } else {
                    &mut deps.unknown_condition
                };
                push(into, value);
            } else if key == "optionaldeps" && decided {
                deps.optional.clear();
                push(&mut deps.optional, value);
            }
        }
        deps.unknown_condition
            .retain(|name| !deps.required.contains(name));
        deps
    }

    /// The title the client of `rules` shows: the last `Title` line it reads.
    pub fn title_for(&self, rules: &ClientRules) -> Option<&str> {
        let names = loaded::client_names(rules);
        let view = View::of(rules, &names);
        self.lines.iter().rev().find_map(|d| match view.read(d) {
            Read::Applies(key, value) if key.eq_ignore_ascii_case("title") => Some(value),
            _ => None,
        })
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
        assert_eq!(toc.title.as_deref(), Some("Second"), "the last line wins");
        assert_eq!(
            toc.issues,
            [TocIssue::DuplicateDirective {
                key: "TITLE".into(),
                line: 2
            }]
        );
        assert_eq!((toc.directives.len(), toc.lines.len()), (1, 2));
        assert!(toc.files.is_empty());

        // Lines with inline directives are no particular client's: never duplicates.
        let toc = parse(
            "## Title: A [AllowLoadGameType standard]\n## Title: B [AllowLoadGameType vanilla]\n## Title: C\n## Title: D\n",
        );
        assert_eq!(toc.title.as_deref(), Some("D"));
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
    fn inline_directives_are_recognized_anywhere_in_a_line() {
        let toc = parse(
            "## Title: My AddOn in Classic [AllowLoadGameType classic]\n\
             ## Interface [allowloadgametype Vanilla]: 11509\n\
             ## X-Odd: a [AllowLoadGameType mists][ExcludeLoadGameType camelot]\n\
             ## Notes: [AllowLoad Game] both\n\
             ## Title: BigWigs [|cffeda55fCore|r]\n\
             ## Notes [Game:Mainline]: colon inside\n\
             ## Version: 1 [AllowLoadGameType]\n\
             ## Author [AllowLoadGameType a:b]: Someone\n",
        );
        let lines: Vec<(&str, &str, &str, &str, Vec<ConditionKind>)> = toc
            .lines
            .iter()
            .map(|d| {
                (
                    d.key.as_str(),
                    d.value.as_str(),
                    d.raw_key.as_str(),
                    d.raw_value.as_str(),
                    d.conditions.iter().map(|c| c.kind.clone()).collect(),
                )
            })
            .collect();
        let allow = ConditionKind::AllowLoadGameType;
        assert_eq!(
            lines,
            [
                (
                    "Title",
                    "My AddOn in Classic",
                    "Title",
                    "My AddOn in Classic [AllowLoadGameType classic]",
                    vec![allow.clone()]
                ),
                (
                    "Interface",
                    "11509",
                    "Interface [allowloadgametype Vanilla]",
                    "11509",
                    vec![allow.clone()]
                ),
                (
                    "X-Odd",
                    "a",
                    "X-Odd",
                    "a [AllowLoadGameType mists][ExcludeLoadGameType camelot]",
                    vec![allow.clone(), ConditionKind::ExcludeLoadGameType]
                ),
                (
                    "Notes",
                    "both",
                    "Notes",
                    "[AllowLoad Game] both",
                    vec![ConditionKind::AllowLoad]
                ),
                // Bracket text that is no inline directive stays.
                (
                    "Title",
                    "BigWigs [|cffeda55fCore|r]",
                    "Title",
                    "BigWigs [|cffeda55fCore|r]",
                    vec![]
                ),
                (
                    "Notes [Game:Mainline]",
                    "colon inside",
                    "Notes [Game:Mainline]",
                    "colon inside",
                    vec![]
                ),
                // A known kind without values cannot be decided.
                (
                    "Version",
                    "1",
                    "Version",
                    "1 [AllowLoadGameType]",
                    vec![ConditionKind::Other("AllowLoadGameType".into())]
                ),
                // A colon inside an inline directive is not the key's.
                (
                    "Author",
                    "Someone",
                    "Author [AllowLoadGameType a:b]",
                    "Someone",
                    vec![allow]
                ),
            ]
        );
        assert_eq!(
            toc.lines[0].conditions[0].values,
            ["classic"],
            "values lowercased"
        );
        assert_eq!(
            toc.title.as_deref(),
            Some("BigWigs [|cffeda55fCore|r]"),
            "the only title without inline directives"
        );
        assert_eq!(toc.interface_raw, None);
    }

    #[test]
    fn for_one_key_the_last_line_read_wins_and_lists_add_up() {
        let toc = parse(
            "## Interface: 120105\n\
             ## Interface: 11509 [AllowLoadGameType vanilla]\n\
             ## Dependencies: Alpha\n\
             ## Dependencies: Beta, alpha\n\
             ## RequiredDeps: Gamma [AllowLoadGameType standard]\n\
             ## OptionalDeps: Old\n\
             ## OptionalDeps: New\n\
             ## SavedVariables: A\n\
             ## SavedVariables: B\n\
             ## AllowLoadGameType: standard\n\
             ## AllowLoadGameType: vanilla [AllowLoadGameType vanilla]\n",
        );
        // No particular client: the lines without inline directives.
        assert_eq!(toc.interfaces, [120_105]);
        assert_eq!(toc.dependencies, ["Alpha", "Beta"]);
        assert_eq!(toc.optional_deps, ["New"]);
        assert_eq!(toc.saved_variables, ["A", "B"]);
        assert_eq!(toc.allow_load_game_type, ["standard"]);
        assert_eq!(
            toc.issues,
            [TocIssue::DuplicateDirective {
                key: "OptionalDeps".into(),
                line: 7
            }],
            "lists add up without duplicates"
        );

        let retail = toc.for_client(&rules_of("12.1.5.70391", false));
        assert_eq!(retail.interfaces, [120_105]);
        assert_eq!(retail.dependencies, ["Alpha", "Beta", "Gamma"]);
        assert_eq!(retail.allow_load_game_type, ["standard"]);
        let era = toc.for_client(&rules_of("1.15.9.64520", false));
        assert_eq!(era.interfaces, [11_509], "the last line that applies");
        assert_eq!(era.dependencies, ["Alpha", "Beta"]);
        assert_eq!(era.allow_load_game_type, ["standard", "vanilla"]);
        assert_eq!(era.lines, toc.lines, "never adds or drops a line");
        assert_eq!(era.issues.len(), 2, "{:?}", era.issues);

        // A client without inline directives reads every line as written.
        let old = toc.for_client(&rules_of("10.2.7", false));
        assert_eq!(
            old.interface_raw.as_deref(),
            Some("11509 [AllowLoadGameType vanilla]")
        );
        assert_eq!(old.interface_leading, Some(11_509));
        assert_eq!(
            old.dependencies,
            ["Alpha", "Beta", "Gamma [AllowLoadGameType standard]"]
        );

        // A line that cannot be decided offline sets nothing.
        let toc = parse(
            "## Interface: 120105\n## Interface: 11509 [AllowLoadTextLocale enUS]\n## Title: T [AllowLoadTextLocale deDE]\n",
        );
        let retail = toc.for_client(&rules_of("12.1.5.70391", false));
        assert_eq!(
            (retail.interfaces.as_slice(), retail.title),
            (&[120_105][..], None)
        );
    }

    #[test]
    fn inline_directive_kinds_admit_or_cannot_tell() {
        let names: Vec<String> = ["standard", "mainline"].map(str::to_owned).to_vec();
        for (raw, admits) in [
            ("AllowLoadGameType standard", Some(true)),
            ("AllowLoadGameType vanilla", Some(false)),
            ("ExcludeLoadGameType mainline", Some(false)),
            ("ExcludeLoadGameType classic", Some(true)),
            ("AllowLoad Game", Some(true)),
            ("AllowLoad both", Some(true)),
            ("AllowLoad Glue", Some(false)),
            ("AllowLoadEnvironment Global", Some(true)),
            ("AllowLoadEnvironment Secure", Some(false)),
            ("LoadIntoEnvironment Secure", Some(true)),
            ("AllowLoadTextLocale enUS", None),
            ("Bootstrap", None),
            ("AllowLoadGameType", None),
        ] {
            assert_eq!(Condition::parse(raw).admits(&names), admits, "{raw}");
        }
    }

    #[test]
    fn file_lines() {
        let toc = parse(
            "libs\\LibStub\\LibStub.lua\nLocales\\[TextLocale].lua\nRetail.lua [AllowLoadGameType standard]\nCore-[Family]-[Game].lua\n  Spaced.lua  \nBoth.lua [AllowLoadGameType mists][ExcludeLoadGameType camelot]\nLocales\\[TextLocale]\n",
        );
        let files: Vec<(&str, Vec<&str>, Vec<&str>)> = toc
            .files
            .iter()
            .map(|f| {
                (
                    f.path.as_str(),
                    f.conditions.iter().map(String::as_str).collect(),
                    f.path_vars.iter().map(String::as_str).collect(),
                )
            })
            .collect();
        assert_eq!(
            files,
            [
                ("libs/LibStub/LibStub.lua", vec![], vec![]),
                ("Locales/[TextLocale].lua", vec![], vec!["TextLocale"]),
                ("Retail.lua", vec!["AllowLoadGameType standard"], vec![]),
                ("Core-[Family]-[Game].lua", vec![], vec!["Family", "Game"]),
                ("Spaced.lua", vec![], vec![]),
                (
                    "Both.lua",
                    vec!["AllowLoadGameType mists", "ExcludeLoadGameType camelot"],
                    vec![]
                ),
                ("Locales/[TextLocale]", vec![], vec!["TextLocale"]),
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
        assert_eq!(modern.files[3].conditions, ["AllowLoadGameType standard"]);
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
                vec!["Details".into()]
            ),
            "the last OptionalDeps line read"
        );
        assert_eq!(
            on(&titan),
            (
                vec!["LibStub".into(), "ace3".into()],
                vec!["Details".into()]
            )
        );

        // A line that cannot be decided offline is never required; an optional one is
        // dropped. Bracket text that is no inline directive is part of the key.
        let toc = parse(
            "## Dependencies: Future, B [AllowLoadTextLocale enUS]\n## Dependencies: B\n\
             ## OptionalDeps: Opt [AllowLoadTextLocale enUS]\n## Dependencies [Game:Mainline]: Odd\n",
        );
        let deps = toc.dependencies_for(&retail);
        assert_eq!(names(&deps.required), ["B"]);
        assert_eq!(
            names(&deps.unknown_condition),
            ["Future"],
            "B is required anyway"
        );
        assert!(deps.optional.is_empty());

        // Repeated lines add up, in either form.
        let toc = parse(
            "## Dependencies: Alpha\n## Dependencies: Beta\n## Dependencies: Ghost [AllowLoadGameType classic]\n",
        );
        assert_eq!(
            names(&toc.dependencies_for(&retail).required),
            ["Alpha", "Beta"]
        );
        assert_eq!(
            names(&toc.dependencies_for(&era).required),
            ["Alpha", "Beta", "Ghost"]
        );
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
        assert_eq!(plain.title_for(&era), Some("Era"), "the last line read");
    }

    /// The shapes publishers ship (synthetic files): titles and optional dependencies
    /// at the end of the line, one per client.
    #[test]
    fn publisher_shaped_tocs_read_per_client() {
        let elvui = parse_toc(include_bytes!(
            "../tests/fixtures/toc/parser/elvui-shaped.toc"
        ));
        let libraries = parse_toc(include_bytes!(
            "../tests/fixtures/toc/parser/libraries-shaped.toc"
        ));
        let gw2 = parse_toc(include_bytes!(
            "../tests/fixtures/toc/parser/gw2-shaped.toc"
        ));
        assert_eq!(elvui.title, None, "no title without inline directives");
        // (client, title, Libraries' optional dependencies, GW2's)
        let cases = [
            (
                "12.1.5.70391",
                Some("Shaped Midnight"),
                vec![],
                vec!["Masque"],
            ),
            (
                "5.5.4.65000",
                Some("Shaped Mists"),
                vec!["LibHealComm-4.0"],
                vec![],
            ),
            (
                "3.80.2.65000",
                Some("Shaped Wrath"),
                vec!["LibHealComm-4.0"],
                vec!["Questie"],
            ),
            (
                "2.5.6.65000",
                Some("Shaped TBC"),
                vec!["LibHealComm-4.0"],
                vec!["Questie"],
            ),
            (
                "1.15.9.64520",
                Some("Shaped Classic"),
                vec!["LibHealComm-4.0"],
                vec![],
            ),
            (
                "1.60.1.70245",
                Some("Shaped Forever"),
                vec![],
                vec!["Masque"],
            ),
        ];
        for (version, title, libs, gw2_optional) in cases {
            let rules = rules_of(version, false);
            assert_eq!(elvui.title_for(&rules), title, "{version}");
            assert_eq!(
                elvui.for_client(&rules).title.as_deref(),
                title,
                "{version}"
            );
            assert_eq!(
                names(&libraries.dependencies_for(&rules).optional),
                libs,
                "{version}"
            );
            assert_eq!(
                names(&gw2.dependencies_for(&rules).optional),
                gw2_optional,
                "{version}"
            );
        }
        // A client without inline directives reads the last line as written.
        let legacy = rules_of("1.12.1", true);
        assert!(
            elvui
                .title_for(&legacy)
                .is_some_and(|t| t.starts_with("Shaped Forever [AllowLoadGameType camelot]"))
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
            for rules in crate::rules::RulesTable::builtin().client_rules() {
                prop_assert_eq!(&toc.for_client(rules).lines, &toc.lines);
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
