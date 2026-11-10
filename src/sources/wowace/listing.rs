//! What WowAce's Subversion server (Apache mod_dav_svn) and a project's packaging files
//! say, read from untrusted bytes (M3-D15): a directory listing (the HTML or the XML
//! index), the tag list a `PROPFIND` answers with each tag's last-changed revision, the
//! newest stable tag, `.pkgmeta`, and the packager's keywords in a TOC, Lua or XML file.
//!
//! Every function is pure and bounded, and none panics on any input. They are hand-written
//! tolerant scanners for the few elements and keys they need, with no XML or YAML crate:
//! what they do not know they ignore, as long as what they need is there. What a project
//! needs that the source cannot do without the packager is reported by name, so it is
//! refused, never installed half-built.

use std::cmp::Ordering;
use std::collections::BTreeSet;

use crate::limits::WOWACE_TREE_MAX_FILES;

/// One entry of a directory listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub dir: bool,
}

/// A tag and its last-changed revision: the immutable root a fetch pins, and the
/// release's identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagRev {
    pub tag: String,
    pub rev: u32,
}

/// Why a listing or a tag list was not read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ListingError {
    /// Neither index format, nor a multistatus: an error page, a login page, a truncated
    /// document.
    #[error("the server's answer is not a directory listing")]
    NotAListing,
    #[error("the listing names more than {max} entries")]
    TooManyEntries { max: usize },
    /// Two names equal ignoring case: WoW loads folders case-insensitively.
    #[error("the listing names {name:?} twice, ignoring case")]
    DuplicateName { name: String },
    /// A name with `/`, `..` or a control character, or an empty one.
    #[error("the listing names {name:?}, which is not a plain file or folder name")]
    BadName { name: String },
}

impl ListingError {
    /// The reason's wire name.
    pub const fn wire(&self) -> &'static str {
        match self {
            Self::NotAListing => "not_a_listing",
            Self::TooManyEntries { .. } => "too_many_entries",
            Self::DuplicateName { .. } => "duplicate_name",
            Self::BadName { .. } => "bad_name",
        }
    }
}

/// What a name in a listing is.
enum Name {
    /// A plain file or folder name.
    Plain(String),
    /// A dotfile, or the parent link: never packaged, skipped.
    Skipped,
}

/// A listed name, checked: dotfiles (and `.`, `..`) are skipped; a name that is empty, or
/// has `/`, `..` or a control character, is refused.
fn name(raw: &str) -> Result<Name, ListingError> {
    if raw.starts_with('.') {
        return Ok(Name::Skipped);
    }
    let bad = raw.is_empty()
        || raw.contains('/')
        || raw.contains("..")
        || raw.chars().any(char::is_control);
    if bad {
        return Err(ListingError::BadName {
            name: raw.to_owned(),
        });
    }
    Ok(Name::Plain(raw.to_owned()))
}

/// A mod_dav_svn directory listing: the HTML index (`<li><a href="name/">…</a></li>`, a
/// trailing `/` marking a folder) or the XML index (`<dir name="…"/>`, `<file name="…"/>`).
pub fn parse_listing(body: &[u8]) -> Result<Vec<Entry>, ListingError> {
    let text = String::from_utf8_lossy(body);
    let raw = match xml_index(&text) {
        Some(raw) => raw,
        None => html_index(&text).ok_or(ListingError::NotAListing)?,
    };
    let mut entries = Vec::new();
    let mut seen = BTreeSet::new();
    for (raw, dir) in raw {
        let Name::Plain(name) = name(&raw)? else {
            continue;
        };
        if entries.len() == WOWACE_TREE_MAX_FILES {
            return Err(ListingError::TooManyEntries {
                max: WOWACE_TREE_MAX_FILES,
            });
        }
        if !seen.insert(name.to_lowercase()) {
            return Err(ListingError::DuplicateName { name });
        }
        entries.push(Entry { name, dir });
    }
    Ok(entries)
}

/// The XML index's names, when `text` is one: an `<index>` element, closed.
fn xml_index(text: &str) -> Option<Vec<(String, bool)>> {
    let start = text.find("<index")?;
    let inner = &text[start..];
    let end = inner.find("</index>").or_else(|| {
        // An empty directory's index may close itself.
        let open = inner.find('>')?;
        inner[..open].ends_with('/').then_some(open)
    })?;
    let inner = &inner[..end];
    let mut found = Vec::new();
    for (tag, dir) in [("<dir ", true), ("<file ", false)] {
        let mut rest = inner;
        while let Some(at) = rest.find(tag) {
            rest = &rest[at + tag.len()..];
            let element = rest.split('>').next().unwrap_or_default();
            if let Some(name) = attribute(element, "name") {
                found.push((name, dir));
            }
        }
    }
    Some(found)
}

/// The HTML index's names, when `text` is one: a `Revision N:` heading, then a closed list
/// of links.
fn html_index(text: &str) -> Option<Vec<(String, bool)>> {
    let heading = text.find("Revision ")?;
    let after = &text[heading + "Revision ".len()..];
    let digits = after.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 || after.as_bytes().get(digits) != Some(&b':') {
        return None;
    }
    let list = after.find("<ul>")?;
    let list = &after[list..];
    let list = &list[..list.find("</ul>")?];
    let mut found = Vec::new();
    let mut rest = list;
    while let Some(at) = rest.find("<a ") {
        rest = &rest[at + "<a ".len()..];
        let element = rest.split('>').next().unwrap_or_default();
        let Some(href) = attribute(element, "href") else {
            continue;
        };
        let href = unescape_percent(&href);
        let (name, dir) = match href.strip_suffix('/') {
            Some(name) => (name.to_owned(), true),
            None => (href, false),
        };
        found.push((name, dir));
    }
    Some(found)
}

/// The value of `key="…"` (or `'…'`) in an element's attributes, entities decoded.
fn attribute(element: &str, key: &str) -> Option<String> {
    let mut rest = element;
    loop {
        let at = rest.find(key)?;
        let before = rest[..at].chars().next_back();
        rest = &rest[at + key.len()..];
        // A whole attribute name, not the end of a longer one (`xname=`).
        if before.is_some_and(|c| !c.is_whitespace()) {
            continue;
        }
        let Some(value) = rest.trim_start().strip_prefix('=') else {
            continue;
        };
        let value = value.trim_start();
        let quote = value.chars().next()?;
        if quote != '"' && quote != '\'' {
            return None;
        }
        let value = &value[1..];
        let end = value.find(quote)?;
        return Some(unescape(&value[..end]));
    }
}

/// XML and HTML entities: the five named ones and numeric references.
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let Some(end) = rest.find(';').filter(|end| *end <= 10) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[1..end];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => entity
                .strip_prefix("#x")
                .or_else(|| entity.strip_prefix("#X"))
                .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                .or_else(|| entity.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// `%XX` escapes decoded (invalid ones kept as written), as bytes, read back lossily.
pub(super) fn unescape_percent(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| char::from(b).to_digit(16);
        match (bytes[i], bytes.get(i + 1), bytes.get(i + 2)) {
            (b'%', Some(&h), Some(&l)) if hex(h).is_some() && hex(l).is_some() => {
                let value = hex(h).unwrap_or(0) * 16 + hex(l).unwrap_or(0);
                out.push(u8::try_from(value).unwrap_or(b'%'));
                i += 3;
            }
            (b, _, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The tags a `PROPFIND` (`Depth: 1`) of `tags/` answers with, each with its
/// `version-name`: the tag directory's last-changed revision. The collection itself is
/// skipped, and so is a response without a readable revision (never guessed). Element
/// prefixes do not matter (`D:`, `lp1:`): only local names are matched.
pub fn parse_tag_revs(body: &[u8]) -> Result<Vec<TagRev>, ListingError> {
    let text = String::from_utf8_lossy(body);
    if elements(&text, "multistatus").is_empty() {
        return Err(ListingError::NotAListing);
    }
    let responses: Vec<(String, Option<u32>)> = elements(&text, "response")
        .into_iter()
        .filter_map(|response| {
            let href = elements(response, "href").into_iter().next()?.trim();
            let rev = elements(response, "version-name")
                .into_iter()
                .next()
                .and_then(|v| v.trim().parse::<u32>().ok());
            Some((unescape_percent(href), rev))
        })
        .collect();
    if responses.is_empty() {
        return Err(ListingError::NotAListing);
    }
    // The collection: the one href every other one is below.
    let segments = |href: &str| href.split('/').filter(|s| !s.is_empty()).count();
    let parent = responses
        .iter()
        .map(|(href, _)| href.as_str())
        .min_by_key(|href| segments(href))
        .map(|href| href.trim_end_matches('/').to_owned());
    let mut tags = Vec::new();
    for (href, rev) in &responses {
        // ubs:ignore — comparing listing paths, not secrets
        if Some(href.trim_end_matches('/')) == parent.as_deref() {
            continue;
        }
        let Some(raw) = href.split('/').rfind(|s| !s.is_empty()) else {
            continue;
        };
        let Name::Plain(tag) = name(&unescape(raw))? else {
            continue;
        };
        let Some(rev) = *rev else { continue };
        if tags.len() == WOWACE_TREE_MAX_FILES {
            return Err(ListingError::TooManyEntries {
                max: WOWACE_TREE_MAX_FILES,
            });
        }
        tags.push(TagRev { tag, rev });
    }
    Ok(tags)
}

/// The inner text of each `<prefix:local …>…</prefix:local>` element (any prefix, or
/// none), in order; one left open is not an element.
fn elements<'a>(text: &'a str, local: &str) -> Vec<&'a str> {
    let mut found = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find('<') {
        rest = &rest[at + 1..];
        if rest.starts_with('/') || rest.starts_with('?') || rest.starts_with('!') {
            continue;
        }
        let name_end = rest
            .find(|c: char| c.is_whitespace() || c == '>' || c == '/')
            .unwrap_or(rest.len());
        let tag = &rest[..name_end];
        let matches = tag.rsplit(':').next() == Some(local);
        if !matches {
            continue;
        }
        let Some(open_end) = rest.find('>') else {
            break;
        };
        if rest[..open_end].ends_with('/') {
            // Self-closed: no inner text.
            found.push("");
            rest = &rest[open_end + 1..];
            continue;
        }
        let body = &rest[open_end + 1..];
        let close = format!("</{tag}>");
        let Some(end) = body.find(&close) else {
            break;
        };
        found.push(&body[..end]);
        rest = &body[end + close.len()..];
    }
    found
}

/// The newest stable tag: `v?N(.N)*` with an optional suffix, where `alpha`, `beta`, `rc`
/// and `pre` (any case) make a pre-release, compared number by number (`v12.1.0` is newer
/// than `v9.9.9`); between equal numbers, a suffixed tag follows the bare one
/// (`v2.5.0-hotfix1`, `v2.5.0a` after `v2.5.0`, ordered by word then number), and
/// between equal versions (`v12.1`, `v12.1.0`), the shorter tag. The revision never
/// decides: a re-tag of an old version can have a high one.
pub fn newest_stable_tag(tags: &[TagRev]) -> Option<&TagRev> {
    tags.iter()
        .filter_map(stable_key)
        .max_by(stable_order)
        .map(|(_, _, t)| t)
}

/// Every stable tag, newest first by [`newest_stable_tag`]'s order: what `info
/// --releases` lists, the first being the one a fetch installs.
pub fn stable_tags_newest_first(tags: &[TagRev]) -> Vec<&TagRev> {
    let mut stable: Vec<StableKey<'_>> = tags.iter().filter_map(stable_key).collect();
    stable.sort_by(|a, b| stable_order(b, a));
    stable.into_iter().map(|(_, _, t)| t).collect()
}

/// A stable tag's numbers and suffix, beside the tag; `None` for any other tag.
type StableKey<'a> = (Vec<&'a str>, Option<(String, String)>, &'a TagRev);

fn stable_key(tag: &TagRev) -> Option<StableKey<'_>> {
    stable_version(&tag.tag).map(|v| (v, suffix_of(&tag.tag), tag))
}

/// Older first: by numbers, then a bare tag before a suffixed one, then the longer tag
/// of two equal versions (`v12.1.0` before `v12.1`).
fn stable_order(a: &StableKey<'_>, b: &StableKey<'_>) -> Ordering {
    let ((va, sa, ta), (vb, sb, tb)) = (a, b);
    compare_versions(va, vb)
        .then_with(|| compare_suffixes(sa.as_ref(), sb.as_ref()))
        .then_with(|| tb.tag.len().cmp(&ta.tag.len()))
}

/// A stable tag's suffix after its numbers, as (word lowercased, digits): `-hotfix2` is
/// `("hotfix", "2")`; `None` for a bare tag.
fn suffix_of(tag: &str) -> Option<(String, String)> {
    let rest = tag.strip_prefix('v').unwrap_or(tag);
    let at = rest.find(|c: char| !(c.is_ascii_digit() || c == '.'))?;
    let suffix = rest[at..]
        .strip_prefix(['-', '_', '+'])
        .unwrap_or(&rest[at..]);
    let letters = suffix
        .find(|c: char| !c.is_ascii_alphabetic())
        .unwrap_or(suffix.len());
    let (word, digits) = suffix.split_at(letters);
    Some((word.to_ascii_lowercase(), digits.to_owned()))
}

/// No suffix before any; then by word, then by number (digit strings of any length).
fn compare_suffixes(a: Option<&(String, String)>, b: Option<&(String, String)>) -> Ordering {
    match (a, b) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (Some((wa, da)), Some((wb, db))) => wa
            .cmp(wb)
            .then_with(|| compare_versions(&[da.as_str()], &[db.as_str()])),
    }
}

/// A stable tag's numeric components, `None` for a pre-release or a tag that is not
/// `v?N(.N)*[[-_+]?letters digits*]`.
fn stable_version(tag: &str) -> Option<Vec<&str>> {
    let rest = tag.strip_prefix('v').unwrap_or(tag);
    let numeric_end = rest
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(rest.len());
    let (numbers, suffix) = rest.split_at(numeric_end);
    let parts: Vec<&str> = numbers.split('.').collect();
    if parts.iter().any(|p| p.is_empty()) {
        return None;
    }
    if !suffix.is_empty() {
        let suffix = suffix.strip_prefix(['-', '_', '+']).unwrap_or(suffix);
        let letters = suffix
            .find(|c: char| !c.is_ascii_alphabetic())
            .unwrap_or(suffix.len());
        let (word, digits) = suffix.split_at(letters);
        if word.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let word = word.to_ascii_lowercase();
        if matches!(word.as_str(), "alpha" | "beta" | "rc" | "pre") {
            return None;
        }
    }
    Some(parts)
}

/// Two versions compared number by number, a missing component as 0; numbers of any
/// length (no overflow: compared as digit strings).
fn compare_versions(a: &[&str], b: &[&str]) -> Ordering {
    let number = |s: &str| -> String { s.trim_start_matches('0').to_owned() };
    for i in 0..a.len().max(b.len()) {
        let x = a.get(i).map_or_else(String::new, |s| number(s));
        let y = b.get(i).map_or_else(String::new, |s| number(s));
        let order = x.len().cmp(&y.len()).then_with(|| x.cmp(&y));
        if order != Ordering::Equal {
            return order;
        }
    }
    Ordering::Equal
}

/// What `.pkgmeta` asks of the packager.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PkgMeta {
    /// `package-as`: the folder the project installs as (the source checks it is one).
    pub package_as: Option<String>,
    /// Every other top-level key the source cannot do without the packager, as written
    /// (`enable-nolib-creation` with its value).
    pub unsupported: Vec<String>,
}

/// The keys that only describe a project (M3-D15): allowed, ignored.
const METADATA_KEYS: &[&str] = &[
    "manual-changelog",
    "changelog-title",
    "license-output",
    "required-dependencies",
    "optional-dependencies",
    "tools-used",
];

/// `.pkgmeta`, read line by line with no YAML parser: `package-as`, and every top-level
/// key outside the metadata-only allowlist (`externals`, `ignore`, `move-folders`, …).
/// Indented lines and list items belong to the key above them; comments and blank lines
/// are ignored.
pub fn parse_pkgmeta(body: &str) -> PkgMeta {
    let mut meta = PkgMeta::default();
    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if line.starts_with(char::is_whitespace) || trimmed.starts_with('-') {
            continue;
        }
        let Some((key, value)) = trimmed.split_once(':') else {
            meta.unsupported.push(trimmed.to_owned());
            continue;
        };
        let key = key.trim();
        let value = unquote(value.trim());
        match key {
            "package-as" => meta.package_as = Some(value.to_owned()),
            "enable-nolib-creation" if value.eq_ignore_ascii_case("no") => {}
            "enable-nolib-creation" => meta.unsupported.push(format!("{key}: {value}")),
            _ if METADATA_KEYS.contains(&key) || key.starts_with("wowi-") => {}
            _ => meta.unsupported.push(key.to_owned()),
        }
    }
    meta
}

/// A YAML scalar without its matching quotes.
fn unquote(value: &str) -> &str {
    for quote in ['"', '\''] {
        if let Some(inner) = value
            .strip_prefix(quote)
            .and_then(|v| v.strip_suffix(quote))
        {
            return inner;
        }
    }
    value
}

/// What the packager's keywords in one file say.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeywordScan {
    /// Lines (from 1) of a `.toc` holding `@project-version@`, which the source replaces
    /// with the tag.
    pub project_version_lines: Vec<usize>,
    /// Every other keyword or build-type block, as `<path>:<line>: <keyword>`
    /// (`Core.lua:12: --@debug@`).
    pub unsupported: Vec<String>,
}

/// The packager's keywords in the file at `path` (its extension says which block marker
/// it uses: `--@` in Lua, `<!--@` in XML, `#@` in a TOC; any for another file): keywords
/// `@word@` and `@word(…)@` anywhere in a line, and block markers at a line's start (after
/// any indentation). Only `@project-version@` in a `.toc` is supported. Errs towards
/// reporting: a false positive refuses a project by name, a false negative would
/// install it half-built.
pub fn scan_keywords(path: &str, body: &[u8]) -> KeywordScan {
    let text = String::from_utf8_lossy(body);
    let ext = path
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
        .unwrap_or_default();
    let markers: &[&str] = match ext.as_str() {
        "lua" => &["--@"],
        "xml" => &["<!--@"],
        "toc" => &["#@"],
        _ => &["--@", "<!--@", "#@"],
    };
    let mut scan = KeywordScan::default();
    for (index, line) in text.split('\n').enumerate() {
        let number = index + 1;
        let start = line.trim_start();
        if let Some(marker) = markers.iter().find(|m| start.starts_with(**m)) {
            let block = block_marker(start, marker);
            scan.unsupported.push(format!("{path}:{number}: {block}"));
            continue;
        }
        for keyword in keywords(line) {
            if matches!(keyword, "@project-version@") && matches!(ext.as_str(), "toc") {
                scan.project_version_lines.push(number);
            } else {
                scan.unsupported.push(format!("{path}:{number}: {keyword}"));
            }
        }
    }
    scan
}

/// A block marker at `start`: the marker and its word up to the closing `@` (with an XML
/// comment's `-->`), or the rest of the line when it never closes.
fn block_marker(start: &str, marker: &str) -> String {
    let after = &start[marker.len()..];
    let block = match after.find('@') {
        Some(end) => {
            let mut block = format!("{marker}{}", &after[..=end]);
            if marker == "<!--@" && after[end + 1..].starts_with("-->") {
                block.push_str("-->");
            }
            block
        }
        None => start.trim_end().to_owned(),
    };
    block.chars().take(80).collect()
}

/// The `@word@` and `@word(…)@` keywords in `line`, `word` being
/// `[A-Za-z][A-Za-z0-9_-]*`. A lone `@` (an address in a comment) is none.
fn keywords(line: &str) -> Vec<&str> {
    let bytes = line.as_bytes();
    let mut found = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'@' {
            i += 1;
            continue;
        }
        let start = i;
        let mut j = i + 1;
        if !bytes.get(j).is_some_and(u8::is_ascii_alphabetic) {
            i += 1;
            continue;
        }
        while bytes
            .get(j)
            .is_some_and(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
        {
            j += 1;
        }
        if bytes.get(j) == Some(&b'(') {
            match bytes[j..].iter().position(|b| *b == b')') {
                Some(close) => j += close + 1,
                None => {
                    i += 1;
                    continue;
                }
            }
        }
        if bytes.get(j) == Some(&b'@') {
            // Byte offsets of ASCII bytes: char boundaries.
            found.push(&line[start..=j]);
            i = j + 1;
        } else {
            i += 1;
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// mod_dav_svn's HTML index of BugGrabber v12.1.0, as documented.
    const HTML: &str = r#"<html><head><title>wow - Revision 401: /bug-grabber/tags/v12.1.0</title></head>
<body>
 <h2>wow - Revision 401: /bug-grabber/tags/v12.1.0</h2>
 <ul>
  <li><a href="../">..</a></li>
  <li><a href=".pkgmeta">.pkgmeta</a></li>
  <li><a href="%21BugGrabber.toc">!BugGrabber.toc</a></li>
  <li><a href="BugGrabber.lua">BugGrabber.lua</a></li>
  <li><a href="Libs/">Libs/</a></li>
  <li><a href="load.xml">load.xml</a></li>
 </ul>
 <hr noshade><em>Powered by <a href="http://subversion.apache.org/">Apache Subversion</a> version 1.9.3 (r1718519).</em>
</body></html>"#;

    /// The XML index of the same directory.
    const XML: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<?xml-stylesheet type="text/xsl" href="/svnindex.xsl"?>
<svn version="1.9.3 (r1718519)" href="http://subversion.apache.org/">
  <index rev="401" path="/bug-grabber/tags/v12.1.0" base="wow">
    <updir href="../"/>
    <file name=".pkgmeta" href=".pkgmeta"/>
    <file name="!BugGrabber.toc" href="%21BugGrabber.toc"/>
    <file name="BugGrabber.lua" href="BugGrabber.lua"/>
    <dir name="Libs" href="Libs/"/>
    <file name="load.xml" href="load.xml"/>
  </index>
</svn>"#;

    /// The `PROPFIND Depth: 1` multistatus of `tags/`: the collection and two tags.
    const PROPFIND: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<D:multistatus xmlns:D="DAV:" xmlns:ns0="DAV:">
<D:response xmlns:lp1="DAV:" xmlns:lp2="http://subversion.tigris.org/xmlns/dav/">
<D:href>/wow/bug-grabber/tags/</D:href>
<D:propstat><D:prop>
<lp1:resourcetype><D:collection/></lp1:resourcetype>
<lp1:version-name>401</lp1:version-name>
</D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat>
</D:response>
<D:response xmlns:lp1="DAV:" xmlns:lp2="http://subversion.tigris.org/xmlns/dav/">
<D:href>/wow/bug-grabber/tags/v12.0.9/</D:href>
<D:propstat><D:prop>
<lp1:resourcetype><D:collection/></lp1:resourcetype>
<lp1:version-name>369</lp1:version-name>
</D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat>
</D:response>
<D:response xmlns:lp1="DAV:" xmlns:lp2="http://subversion.tigris.org/xmlns/dav/">
<D:href>/wow/bug-grabber/tags/v12.1.0/</D:href>
<D:propstat><D:prop>
<lp1:resourcetype><D:collection/></lp1:resourcetype>
<lp1:version-name>401</lp1:version-name>
</D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat>
</D:response>
</D:multistatus>"#;

    fn names(entries: &[Entry]) -> Vec<(&str, bool)> {
        entries.iter().map(|e| (e.name.as_str(), e.dir)).collect()
    }

    const EXPECTED: [(&str, bool); 4] = [
        ("!BugGrabber.toc", false),
        ("BugGrabber.lua", false),
        ("Libs", true),
        ("load.xml", false),
    ];

    #[test]
    fn both_index_formats_read_the_same_tree_skipping_dotfiles_and_the_parent() {
        let html = parse_listing(HTML.as_bytes()).unwrap();
        assert_eq!(names(&html), EXPECTED);
        let mut xml = parse_listing(XML.as_bytes()).unwrap();
        xml.sort_by(|a, b| a.name.cmp(&b.name));
        let mut expected = EXPECTED.to_vec();
        expected.sort_unstable();
        assert_eq!(names(&xml), expected);
        let empty = r#"<svn><index rev="5" path="/x" base="wow"/></svn>"#;
        assert_eq!(parse_listing(empty.as_bytes()), Ok(Vec::new()));
    }

    #[test]
    fn bad_names_duplicates_and_size_fail_by_name() {
        let html = |items: &str| {
            format!("<title>wow - Revision 9: /x</title><ul>{items}</ul>").into_bytes()
        };
        for bad in ["a%2Fb", "a..b", "a%01b", "sub/dir/"] {
            let body = html(&format!("<li><a href=\"{bad}\">x</a></li>"));
            assert!(
                matches!(parse_listing(&body), Err(ListingError::BadName { .. })),
                "{bad}"
            );
        }
        let body = html(r#"<li><a href="Core.lua">a</a></li><li><a href="core.lua">b</a></li>"#);
        assert_eq!(
            parse_listing(&body),
            Err(ListingError::DuplicateName {
                name: "core.lua".to_owned()
            })
        );
        let many: String = (0..=WOWACE_TREE_MAX_FILES)
            .map(|n| format!("<li><a href=\"f{n}.lua\">f</a></li>"))
            .collect();
        assert_eq!(
            parse_listing(&html(&many)),
            Err(ListingError::TooManyEntries {
                max: WOWACE_TREE_MAX_FILES
            })
        );
        for not in [
            "<html><head><title>404 Not Found</title></head><body><ul><li>x</li></ul></body></html>",
            "",
            &HTML[..HTML.len() / 2],
            &XML[..XML.len() / 2],
        ] {
            assert_eq!(
                parse_listing(not.as_bytes()),
                Err(ListingError::NotAListing),
                "{not}"
            );
        }
    }

    #[test]
    fn a_propfind_gives_each_tags_last_changed_revision() {
        let tags = parse_tag_revs(PROPFIND.as_bytes()).unwrap();
        assert_eq!(
            tags,
            [
                TagRev {
                    tag: "v12.0.9".into(),
                    rev: 369
                },
                TagRev {
                    tag: "v12.1.0".into(),
                    rev: 401
                }
            ]
        );
        // Another prefix for the same elements reads the same.
        let other = PROPFIND.replace("D:", "a:").replace("lp1:", "b:");
        assert_eq!(parse_tag_revs(other.as_bytes()).unwrap(), tags);
        // A response without a revision is skipped, never guessed.
        let missing = PROPFIND.replacen("<lp1:version-name>369</lp1:version-name>", "", 1);
        assert_eq!(parse_tag_revs(missing.as_bytes()).unwrap().len(), 1);
        let bad = PROPFIND.replace("v12.0.9", "a..b");
        assert!(matches!(
            parse_tag_revs(bad.as_bytes()),
            Err(ListingError::BadName { .. })
        ));
        for not in [
            "",
            "<html>error</html>",
            "<D:multistatus xmlns:D=\"DAV:\"></D:multistatus>",
        ] {
            assert_eq!(
                parse_tag_revs(not.as_bytes()),
                Err(ListingError::NotAListing),
                "{not}"
            );
        }
    }

    fn tag(tag: &str, rev: u32) -> TagRev {
        TagRev {
            tag: tag.into(),
            rev,
        }
    }

    #[test]
    fn stable_tags_are_listed_newest_first_as_the_newest_is_chosen() {
        let names = |tags: &[TagRev]| -> Vec<String> {
            stable_tags_newest_first(tags)
                .into_iter()
                .map(|t| t.tag.clone())
                .collect()
        };
        let cases: [(&str, Vec<TagRev>, &[&str]); 4] = [
            (
                "numbers decide, never the revision; pre-releases and odd tags are left out",
                vec![
                    tag("v9.9.9", 900),
                    tag("v12.1.0", 401),
                    tag("v12.1.0-beta1", 399),
                    tag("latest", 950),
                    tag("v12.0.9", 369),
                ],
                &["v12.1.0", "v12.0.9", "v9.9.9"],
            ),
            (
                "a hotfix after its base, the later hotfix first",
                vec![
                    tag("v2.5.0", 10),
                    tag("v2.5.0-hotfix2", 12),
                    tag("v2.5.0-hotfix10", 11),
                ],
                &["v2.5.0-hotfix10", "v2.5.0-hotfix2", "v2.5.0"],
            ),
            (
                "equal versions: the shorter tag first",
                vec![tag("v12.1.0", 401), tag("v12.1", 500)],
                &["v12.1", "v12.1.0"],
            ),
            ("no stable tag", vec![tag("1.0rc2", 1)], &[]),
        ];
        for (name, tags, expected) in cases {
            assert_eq!(names(&tags), expected, "{name}: {tags:?}");
            assert_eq!(
                stable_tags_newest_first(&tags).first().copied(),
                newest_stable_tag(&tags),
                "{name}: the first listed is the one a fetch installs"
            );
        }
    }

    #[test]
    fn the_newest_stable_tag_is_numeric_and_ignores_pre_releases() {
        let tags = [
            tag("v12.0.9", 369),
            tag("v12.1.0", 401),
            tag("v12.1.0-beta1", 399),
            tag("v9.9.9", 100),
        ];
        assert_eq!(newest_stable_tag(&tags), Some(&tags[1]));
        // The revision never decides, and a tie goes to the shorter tag.
        let tags = [tag("v12.1.0", 401), tag("v12.1", 500), tag("v9.9.9", 900)];
        assert_eq!(newest_stable_tag(&tags), Some(&tags[1]));
        for pre in ["1.0-ALPHA", "1.0rc2", "1.0_pre", "v2.0+beta3"] {
            assert_eq!(newest_stable_tag(&[tag(pre, 1)]), None, "{pre}");
        }
        for odd in ["latest", "v", "v1..2", "1.0-beta-2", "1.0-x.y"] {
            assert_eq!(newest_stable_tag(&[tag(odd, 1)]), None, "{odd}");
        }
        assert_eq!(
            newest_stable_tag(&[tag("v1.0-hotfix2", 1)]).map(|t| t.rev),
            Some(1)
        );
        // A hotfix follows its base version, the later hotfix the earlier.
        let tags = [
            tag("v2.5.0", 10),
            tag("v2.5.0-hotfix2", 12),
            tag("v2.5.0-hotfix10", 11),
        ];
        assert_eq!(newest_stable_tag(&tags), Some(&tags[2]));
        let tags = [tag("1.2.3a", 1), tag("1.2.3", 2)];
        assert_eq!(newest_stable_tag(&tags), Some(&tags[0]));
        let huge = tag(&format!("v{}", "9".repeat(40)), 1);
        assert_eq!(
            newest_stable_tag(&[tag("v12.0", 2), huge.clone()]),
            Some(&huge)
        );
        assert_eq!(newest_stable_tag(&[]), None);
    }

    #[test]
    fn pkgmeta_keeps_package_as_and_names_what_needs_the_packager() {
        let bug_grabber = parse_pkgmeta("package-as: \"!BugGrabber\"\n");
        assert_eq!(bug_grabber.package_as.as_deref(), Some("!BugGrabber"));
        assert!(bug_grabber.unsupported.is_empty());
        assert_eq!(
            parse_pkgmeta("package-as: Plain\n").package_as.as_deref(),
            Some("Plain")
        );
        let refused = parse_pkgmeta(
            "# comment\n\nexternals:\n  Libs/LibStub: https://example.org/x\n    tag: latest\nignore:\n  - README.md\nmove-folders:\n  a: b\nenable-toc-creation: yes\nenable-nolib-creation: yes\nplain-copy:\n  - x\n",
        );
        assert_eq!(
            refused.unsupported,
            [
                "externals",
                "ignore",
                "move-folders",
                "enable-toc-creation",
                "enable-nolib-creation: yes",
                "plain-copy"
            ]
        );
        let allowed = parse_pkgmeta(
            "manual-changelog:\n  filename: CHANGES.md\nchangelog-title: x\nlicense-output: LICENSE.txt\nrequired-dependencies:\n  - libstub\noptional-dependencies: x\ntools-used: x\nwowi-archive-previous: no\nenable-nolib-creation: no\n",
        );
        assert_eq!(allowed, PkgMeta::default());
    }

    #[test]
    fn keywords_only_the_tocs_project_version_is_supported() {
        let toc = "## Interface: 120105\n## Title: !BugGrabber\n## Version: @project-version@\nload.xml\n";
        let scan = scan_keywords("!BugGrabber.toc", toc.as_bytes());
        assert_eq!(
            scan,
            KeywordScan {
                project_version_lines: vec![3],
                unsupported: Vec::new()
            }
        );
        let lua = "local x = 1 -- mail me at a@b.org\nlocal s = \"@\"\n";
        assert_eq!(
            scan_keywords("BugGrabber.lua", lua.as_bytes()),
            KeywordScan::default()
        );
        let xml = "<Ui>\n<Script file=\"BugGrabber.lua\"/>\n</Ui>\n";
        assert_eq!(
            scan_keywords("load.xml", xml.as_bytes()),
            KeywordScan::default()
        );

        let planted = "x = 1\n  --@debug@\nprint(\"@project-revision@\")\nL = @localization(locale=\"enUS\", format=\"lua_additive_table\")@\nv = \"@project-version@\"\n";
        assert_eq!(
            scan_keywords("Core.lua", planted.as_bytes()).unsupported,
            [
                "Core.lua:2: --@debug@",
                "Core.lua:3: @project-revision@",
                "Core.lua:4: @localization(locale=\"enUS\", format=\"lua_additive_table\")@",
                "Core.lua:5: @project-version@",
            ]
        );
        let xml = "<Ui>\n\t<!--@retail@-->\n<Frame/>\n";
        assert_eq!(
            scan_keywords("Frames.xml", xml.as_bytes()).unsupported,
            ["Frames.xml:2: <!--@retail@-->"]
        );
        let toc = "#@no-lib-strip@\nLibs/LibStub.lua\n#@end-no-lib-strip@\n";
        assert_eq!(
            scan_keywords("Foo.toc", toc.as_bytes()).unsupported,
            [
                "Foo.toc:1: #@no-lib-strip@",
                "Foo.toc:3: #@end-no-lib-strip@"
            ]
        );
    }

    /// Every name a parser returns is a plain name.
    fn plain(name: &str) -> bool {
        !name.is_empty()
            && !name.starts_with('.')
            && !name.contains('/')
            && !name.contains("..")
            && !name.chars().any(char::is_control)
    }

    proptest! {
        #[test]
        fn no_parser_panics_and_names_stay_plain(bytes in proptest::collection::vec(any::<u8>(), 0..400)) {
            if let Ok(entries) = parse_listing(&bytes) {
                prop_assert!(entries.iter().all(|e| plain(&e.name)));
            }
            if let Ok(tags) = parse_tag_revs(&bytes) {
                prop_assert!(tags.iter().all(|t| plain(&t.tag)));
                let _ = newest_stable_tag(&tags);
            }
            let _ = parse_pkgmeta(&String::from_utf8_lossy(&bytes));
            for path in ["a.toc", "a.lua", "a.xml"] {
                let _ = scan_keywords(path, &bytes);
            }
        }

        #[test]
        fn shaped_inputs_never_panic(text in "[<>/!?a-zA-Z:=\"' &;#%0-9@().\\-_\n]{0,300}") {
            let _ = parse_listing(text.as_bytes());
            let _ = parse_tag_revs(text.as_bytes());
            let _ = parse_pkgmeta(&text);
            let _ = scan_keywords("x.lua", text.as_bytes());
            let _ = newest_stable_tag(&[TagRev { tag: text.clone(), rev: 1 }]);
        }
    }
}
