//! What of an error's text may leave the process (M3-D21): privacy as a contract on
//! kinds of text, not a list of patterns to redact. An addon can write anything into
//! `error("…")`, a party member's name included, and no pattern recognizes a name. Lua's
//! and the client's own messages are templates whose holes are identifiers from source
//! code, so once the holes are checked they are code-derived and safe.
//!
//! The strict renderer (default and `-v`) shows only that: recognized templates,
//! locations, frames with their function names, and fingerprints. Anything else is
//! [`MessageKind::Custom`], shown by its length only. The raw renderer (`--raw-details`)
//! is best effort: bounded, stripped of UI codes and control characters, with player
//! GUIDs and links and this client's account, realm and character names replaced. It
//! promises no anonymity.

use schemars::JsonSchema;
use serde::Serialize;
use sha2::{Digest, Sha256};

use super::buggrabber::BugEntry;
use super::frames::{FrameKind, frame, location, strip_prefix_ci};
use crate::limits::{
    ERRORS_CHUNK_MAX_CHARS, ERRORS_LOCALS_MAX_CHARS, ERRORS_MESSAGE_MAX_CHARS, ERRORS_STACK_LINES,
    ERRORS_STACK_LINES_VERBOSE,
};
use crate::privacy::WtfPathRedactor;
use crate::text::strip_ui_escapes;

/// What a message is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MessageKind {
    /// One of Lua's own runtime or load messages.
    LuaRuntime,
    /// One of the client's own messages (a protected call, a `LUA_WARNING`).
    Blizzard,
    /// Anything else: an addon's own text, shown only with `--raw-details`.
    Custom,
}

/// A message as strict output may show it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrictMessage {
    pub kind: MessageKind,
    /// The message, only for a recognized template.
    pub text: Option<String>,
    /// The characters of the message's first line after its location.
    pub chars: usize,
    /// `Folder/file.lua:123` (AddOns-relative), or `[custom code]:4` for a code chunk.
    pub location: Option<String>,
}

/// A message's first line as strict output may show it.
pub fn strict_message(raw: &str) -> StrictMessage {
    let first = first_line(raw);
    let (location, body) = match location(first) {
        Some(at) => (strict_location(at), first[at.len()..].trim()),
        None => (None, first.trim()),
    };
    let chars = body.chars().count();
    match template(body) {
        Some(kind) => StrictMessage {
            kind,
            text: Some(clean(body, false)),
            chars,
            location,
        },
        None => StrictMessage {
            kind: MessageKind::Custom,
            text: None,
            chars,
            location,
        },
    }
}

/// A message's first line, its `LUA_WARNING: ` prefix and embedded stack left out.
fn first_line(raw: &str) -> &str {
    let raw = strip_prefix_ci(raw, "LUA_WARNING: ").unwrap_or(raw);
    let raw = raw.split_once("|cffffd200Stack:|r").map_or(raw, |(m, _)| m);
    raw.lines().next().unwrap_or_default()
}

/// A location as strict output shows it.
fn strict_location(at: &str) -> Option<String> {
    let f = frame(at, true);
    match (f.kind, f.path, f.line) {
        (FrameKind::Path, Some(path), Some(line)) => Some(format!("{}:{line}", display_path(path))),
        (FrameKind::Chunk, ..) => Some(match trailing_line(at) {
            Some(line) => format!("[custom code]:{line}"),
            None => "[custom code]".to_owned(),
        }),
        _ => None,
    }
}

/// The line of a location ending `:<line>:`.
fn trailing_line(at: &str) -> Option<u32> {
    let (_, digits) = at.strip_suffix(':')?.rsplit_once(':')?;
    digits.parse().ok()
}

/// A path relative to `AddOns/`, with `/` separators and no UI codes or control
/// characters.
fn display_path(path: &str) -> String {
    let mut start = 0;
    let mut rel = path;
    while let Some(at) = path[start..].find(['/', '\\']) {
        if path[start..start + at].eq_ignore_ascii_case("AddOns") {
            rel = &path[start + at + 1..];
            break;
        }
        start += at + 1;
    }
    clean(&rel.replace('\\', "/"), false)
}

/// `text` without UI codes, and without control characters (but `\n`, and `\t` when
/// `tabs`).
fn clean(text: &str, tabs: bool) -> String {
    strip_ui_escapes(text)
        .chars()
        .filter(|c| !c.is_control() || *c == '\n' || (tabs && *c == '\t'))
        .collect()
}

// ---------------------------------------------------------------------------------------
// The templates.

const TYPES: &[&str] = &[
    "nil", "boolean", "number", "string", "table", "function", "userdata", "thread",
];

/// A message from the template table, and whose it is.
fn template(body: &str) -> Option<MessageKind> {
    let lua: [fn(&mut Cursor<'_>) -> Option<()>; 7] = [
        // `attempt to <verb> a <type> value( (<scope> '<id>'))?` (Lua 5.4 and current
        // clients).
        |c| {
            c.lit("attempt to ")?;
            c.one(VERBS)?;
            c.lit(" a ")?;
            c.one(TYPES)?;
            c.lit(" value")?;
            c.optional(|c| {
                c.lit(" (")?;
                c.one(SCOPES)?;
                c.lit(" ")?;
                c.quoted('\'', is_identifier)?;
                c.lit(")")
            });
            Some(())
        },
        // `attempt to <verb> <scope> '<id>' (a <type> value)` (Lua 5.1: 3.3.5a and older
        // retail).
        |c| {
            c.lit("attempt to ")?;
            c.one(VERBS)?;
            c.lit(" ")?;
            c.one(SCOPES)?;
            c.lit(" ")?;
            c.quoted('\'', is_identifier)?;
            c.lit(" (a ")?;
            c.one(TYPES)?;
            c.lit(" value)")
        },
        // `attempt to compare <type> with <type>`, `attempt to compare two <type> values`.
        |c| {
            c.lit("attempt to compare ")?;
            if c.optional(|c| c.lit("two ")) {
                c.one(TYPES)?;
                return c.lit(" values");
            }
            c.one(TYPES)?;
            c.lit(" with ")?;
            c.one(TYPES)
        },
        // `bad argument #<n> to '<id>' (<type> expected, got <type>|<detail>)`.
        |c| {
            c.lit("bad argument #")?;
            c.digits()?;
            c.lit(" to ")?;
            c.quoted('\'', is_identifier)?;
            c.lit(" (")?;
            if !c.optional(|c| c.one(DETAILS)) {
                c.one(EXPECTED)?;
                c.lit(" expected, got ")?;
                c.one(GOT)?;
            }
            c.lit(")")
        },
        |c| c.one(&["C stack overflow", "stack overflow"]),
        // `'<tok>' expected( (to close '<tok>' at line <n>))? near '<tok>'`.
        |c| {
            c.quoted('\'', is_token)?;
            c.lit(" expected")?;
            c.optional(|c| {
                c.lit(" (to close ")?;
                c.quoted('\'', is_token)?;
                c.lit(" at line ")?;
                c.digits()?;
                c.lit(")")
            });
            c.lit(" near ")?;
            c.quoted('\'', is_token)
        },
        |c| {
            c.lit("unexpected symbol near ")?;
            c.quoted('\'', is_token)
        },
    ];
    let blizzard: [fn(&mut Cursor<'_>) -> Option<()>; 3] = [
        // `[ADDON_ACTION_(BLOCKED|FORBIDDEN)] AddOn '<id>' tried to call the protected
        // function '<id>()'.`
        |c| {
            c.lit("[ADDON_ACTION_")?;
            c.one(&["BLOCKED", "FORBIDDEN"])?;
            c.lit("] AddOn ")?;
            c.quoted('\'', is_identifier)?;
            c.lit(" tried to call the protected function ")?;
            c.quoted('\'', |name| {
                is_identifier(name.strip_suffix("()").unwrap_or(name))
            })?;
            c.lit(".")
        },
        // `Couldn't find inherited node '<id>'` (either quote).
        |c| {
            c.lit("Couldn't find inherited node ")?;
            if c.optional(|c| c.quoted('"', is_identifier)) {
                return Some(());
            }
            c.quoted('\'', is_identifier)
        },
        |c| {
            c.lit("Deferred XML Node object named ")?;
            c.word(is_identifier)?;
            c.lit(" already exists")
        },
    ];
    let whole = |m: fn(&mut Cursor<'_>) -> Option<()>| {
        let mut c = Cursor(body);
        m(&mut c).is_some() && c.0.is_empty()
    };
    if lua.into_iter().any(whole) {
        Some(MessageKind::LuaRuntime)
    } else if blizzard.into_iter().any(whole) {
        Some(MessageKind::Blizzard)
    } else {
        None
    }
}

const VERBS: &[&str] = &[
    "index",
    "call",
    "compare",
    "concatenate",
    "perform arithmetic on",
    "get length of",
];
const SCOPES: &[&str] = &["field", "local", "global", "upvalue", "method", "constant"];
const EXPECTED: &[&str] = &[
    "nil", "boolean", "number", "integer", "string", "table", "function", "userdata", "thread",
    "value",
];
const GOT: &[&str] = &[
    "nil", "boolean", "number", "string", "table", "function", "userdata", "thread", "no value",
];
const DETAILS: &[&str] = &[
    "value expected",
    "invalid option",
    "invalid value",
    "index out of range",
    "out of range",
    "number has no integer representation",
];

/// A cursor over a message, for the template matchers.
struct Cursor<'a>(&'a str);

impl Cursor<'_> {
    fn lit(&mut self, text: &str) -> Option<()> {
        self.0 = self.0.strip_prefix(text)?;
        Some(())
    }

    /// The longest of `options` the message continues with.
    fn one(&mut self, options: &[&str]) -> Option<()> {
        let best = options
            .iter()
            .filter(|o| self.0.starts_with(**o))
            .max_by_key(|o| o.len())?;
        self.0 = &self.0[best.len()..];
        Some(())
    }

    /// `quote`, text `check` accepts, `quote`.
    fn quoted(&mut self, quote: char, check: fn(&str) -> bool) -> Option<()> {
        let rest = self.0.strip_prefix(quote)?;
        let end = rest.find(quote)?;
        check(&rest[..end]).then_some(())?;
        self.0 = &rest[end + quote.len_utf8()..];
        Some(())
    }

    /// A word up to the next space that `check` accepts.
    fn word(&mut self, check: fn(&str) -> bool) -> Option<()> {
        let end = self.0.find(' ').unwrap_or(self.0.len());
        check(&self.0[..end]).then_some(())?;
        self.0 = &self.0[end..];
        Some(())
    }

    fn digits(&mut self) -> Option<()> {
        let n = self.0.bytes().take_while(u8::is_ascii_digit).count();
        (n > 0).then(|| self.0 = &self.0[n..])
    }

    /// Runs `group`, keeping what it read only when it matched; whether it did.
    fn optional(&mut self, group: impl FnOnce(&mut Self) -> Option<()>) -> bool {
        let mut trial = Cursor(self.0);
        let matched = group(&mut trial).is_some();
        if matched {
            self.0 = trial.0;
        }
        matched
    }
}

/// A hole that holds an identifier from source code: `^[A-Za-z_][A-Za-z0-9_.:]*$`, or
/// `?`. A space or a hyphen (`FakeChar-FakeRealm`) is never one.
fn is_identifier(text: &str) -> bool {
    text == "?"
        || text
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
            && text
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b':'))
}

/// A Lua token in a load error: a name, a number, a symbol, or `<eof>`. A string literal
/// is not one.
fn is_token(text: &str) -> bool {
    const SYMBOLS: &[&str] = &[
        "=", "==", "~=", "<", "<=", ">", ">=", "+", "-", "*", "/", "%", "^", "#", "(", ")", "{",
        "}", "[", "]", ";", ":", ",", ".", "..", "...", "<eof>",
    ];
    let name = text
        .bytes()
        .next()
        .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        && text.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
    let number = text.bytes().next().is_some_and(|b| b.is_ascii_digit())
        && text.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.');
    name || number || SYMBOLS.contains(&text)
}

// ---------------------------------------------------------------------------------------
// Frames.

/// A stack as strict output shows it: each frame's AddOns-relative path, line and
/// function name; code chunks as `[custom code]`; runs of `[C]` and tail-call lines
/// folded into one line; at most [`ERRORS_STACK_LINES`] frames
/// ([`ERRORS_STACK_LINES_VERBOSE`] with `verbose`), then how many more there were.
pub fn strict_frames(raw: &str, verbose: bool) -> Vec<String> {
    let limit = if verbose {
        ERRORS_STACK_LINES_VERBOSE
    } else {
        ERRORS_STACK_LINES
    };
    let mut out = Vec::new();
    let (mut shown, mut more) = (0, 0);
    let mut folded = 0;
    let fold = |out: &mut Vec<String>, folded: &mut usize| {
        match *folded {
            0 => {}
            1 => out.push("… (1 C frame)".to_owned()),
            n => out.push(format!("… ({n} C frames)")),
        }
        *folded = 0;
    };
    for line in raw.lines() {
        let f = frame(line, true);
        let shown_as = match (f.kind, f.path, f.line) {
            (FrameKind::CCode | FrameKind::TailCall, ..) => {
                if shown < limit {
                    folded += 1;
                }
                continue;
            }
            (FrameKind::Path, Some(path), Some(line)) => {
                format!("{}:{line}:{}", display_path(path), function_of(f.raw))
            }
            (FrameKind::Chunk, ..) => "[custom code]".to_owned(),
            _ => continue,
        };
        if shown == limit {
            more += 1;
            continue;
        }
        fold(&mut out, &mut folded);
        out.push(shown_as);
        shown += 1;
    }
    fold(&mut out, &mut folded);
    if more > 0 {
        out.push(format!("… ({more} more frames)"));
    }
    out
}

/// What a frame line says after its `:<line>:`, when it is code-derived: ` in function
/// 'X'`, ` in function <file:line>`, ` in main chunk`, ` in method 'X'` and the like.
fn function_of(raw: &str) -> String {
    let Some(rest) = after_line(raw) else {
        return String::new();
    };
    let rest = rest.trim();
    if rest == "in main chunk" {
        return " in main chunk".to_owned();
    }
    for scope in ["function", "method", "local", "upvalue", "field", "global"] {
        let Some(name) = rest
            .strip_prefix("in ")
            .and_then(|r| r.strip_prefix(scope))
            .and_then(|r| r.strip_prefix(' '))
        else {
            continue;
        };
        // `'X'`, or r152's `` `X' ``.
        let quoted = name
            .strip_suffix('\'')
            .and_then(|n| n.strip_prefix('\'').or_else(|| n.strip_prefix('`')));
        if let Some(id) = quoted.filter(|id| is_identifier(id)) {
            return format!(" in {scope} '{id}'");
        }
        // `<Interface/AddOns/Foo/x.lua:38>`
        if let Some(inner) = name.strip_prefix('<').and_then(|n| n.strip_suffix('>'))
            && let Some((path, line)) = inner.rsplit_once(':')
            && line.bytes().all(|b| b.is_ascii_digit())
            && !line.is_empty()
            && !path.starts_with('[')
        {
            return format!(" in {scope} <{}:{line}>", display_path(path));
        }
        return String::new();
    }
    String::new()
}

/// A frame line after its location's `:<line>:`.
fn after_line(raw: &str) -> Option<&str> {
    let bytes = raw.as_bytes();
    let mut from = 0;
    while let Some(at) = raw[from..].find(':') {
        let colon = from + at;
        let digits = bytes[colon + 1..]
            .iter()
            .take_while(|b| b.is_ascii_digit())
            .count();
        let end = colon + 1 + digits;
        if digits > 0 && bytes.get(end) == Some(&b':') {
            return Some(&raw[end + 1..]);
        }
        from = colon + 1;
    }
    None
}

// ---------------------------------------------------------------------------------------
// Fingerprints.

/// A message as an error's identity reads it: the first line (no `LUA_WARNING: `
/// prefix, no embedded stack), without UI codes or control characters, and with library
/// version suffixes of BugGrabber ≤ v12.0.2 data undone (`AceAddon-3.0-13` is
/// `AceAddon-3.0`); line numbers kept.
pub fn normalized(message: &str) -> String {
    let text = clean(first_line(message), false);
    let mut out = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // `<letters>-<digits>.<digits>` followed by `-<digits>`: drop the last part.
        if let Some(cut) = version_suffix(bytes, i) {
            out.push_str(&text[i..cut.0]);
            i = cut.1;
            continue;
        }
        let len = text[i..].chars().next().map_or(1, char::len_utf8);
        out.push_str(&text[i..i + len]);
        i += len;
    }
    out.trim().to_owned()
}

/// At `i`, a `Name-1.0-13` library version: where its `-13` starts and ends.
fn version_suffix(bytes: &[u8], i: usize) -> Option<(usize, usize)> {
    if i > 0 && bytes[i - 1].is_ascii_alphanumeric() {
        return None;
    }
    let count = |from: usize, f: fn(&u8) -> bool| bytes[from..].iter().take_while(|b| f(b)).count();
    let letters = count(i, u8::is_ascii_alphabetic);
    let mut at = i + letters;
    if letters == 0 || bytes.get(at) != Some(&b'-') {
        return None;
    }
    let major = count(at + 1, u8::is_ascii_digit);
    at += 1 + major;
    if major == 0 || bytes.get(at) != Some(&b'.') {
        return None;
    }
    let minor = count(at + 1, u8::is_ascii_digit);
    at += 1 + minor;
    if minor == 0 || bytes.get(at) != Some(&b'-') {
        return None;
    }
    let build = count(at + 1, u8::is_ascii_digit);
    let end = at + 1 + build;
    let ends_word = bytes.get(end).is_none_or(|b| !b.is_ascii_alphanumeric());
    (build > 0 && ends_word).then_some((at, end))
}

/// An error's opaque identity: `e:` and 16 hex digits of the SHA-256 of the normalized
/// message ([`normalized`]) and the location.
pub fn fingerprint(normalized_message: &str, location: Option<&str>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(normalized_message.as_bytes());
    hasher.update([0]);
    hasher.update(location.unwrap_or_default().as_bytes());
    let hex: String = hasher
        .finalize()
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("e:{hex}")
}

// ---------------------------------------------------------------------------------------
// `--raw-details`.

/// An error's free text, made as safe as can be (`--raw-details`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Raw {
    pub message: String,
    pub stack: Vec<String>,
    pub locals: Option<String>,
    /// The contents of the error's `[string "…"]` code chunks.
    pub chunks: Vec<String>,
}

/// `entry`'s text for `--raw-details`: bounded and marked where cut (`…`), UI codes and
/// control characters gone, player GUIDs and links as `<player>`, and this client's
/// account, realm and character names (`names`) as `<account>`, `<realm>` and
/// `<character>`. Best effort: a name it never learned stays.
pub fn raw_details(entry: &BugEntry, names: &WtfPathRedactor) -> Raw {
    let safe = |text: &str, tabs: bool| names.text(&clean(&players(text), tabs));
    let message = cut(
        &safe(first_line(&entry.message), false),
        ERRORS_MESSAGE_MAX_CHARS,
    );
    let stack_text = entry.stack.as_deref().unwrap_or_default();
    let stack: Vec<String> = stack_text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .take(ERRORS_STACK_LINES_VERBOSE)
        .map(|line| chunks_cut(&safe(line, false)))
        .collect();
    let locals = entry
        .locals
        .as_deref()
        .map(|l| cut(&safe(l, true), ERRORS_LOCALS_MAX_CHARS));
    let mut chunks: Vec<String> = Vec::new();
    for text in [entry.message.as_str(), stack_text] {
        for chunk in chunk_contents(text) {
            let chunk = cut(&safe(chunk, false), ERRORS_CHUNK_MAX_CHARS);
            if !chunks.contains(&chunk) {
                chunks.push(chunk);
            }
        }
    }
    Raw {
        message,
        stack,
        locals,
        chunks,
    }
}

/// `text` at most `max` characters, ending `…` when cut.
fn cut(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(max).collect();
    out.push('…');
    out
}

/// `text` with player links (`|Hplayer:…|h[name]|h`) and player GUIDs
/// (`Player-<digits>-<8 hex>`) replaced by `<player>`.
fn players(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while !rest.is_empty() {
        if let Some(link) = rest.strip_prefix("|Hplayer:")
            && let Some((_, after_data)) = link.split_once("|h")
            && let Some((_, after)) = after_data.split_once("|h")
        {
            out.push_str("<player>");
            rest = after;
            continue;
        }
        if let Some(guid) = rest.strip_prefix("Player-") {
            let realm = guid.bytes().take_while(u8::is_ascii_digit).count();
            let id = guid.get(realm + 1..realm + 9);
            if realm > 0
                && guid.as_bytes().get(realm) == Some(&b'-')
                && id.is_some_and(|h| h.bytes().all(|b| b.is_ascii_hexdigit()))
            {
                out.push_str("<player>");
                rest = &guid[realm + 9..];
                continue;
            }
        }
        let len = rest.chars().next().map_or(1, char::len_utf8);
        out.push_str(&rest[..len]);
        rest = &rest[len..];
    }
    out
}

/// The contents of the `[string "…"]` code chunks in `text` (not `@` file chunks).
fn chunk_contents(text: &str) -> Vec<&str> {
    let mut found = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find("[string \"") {
        let inner = &rest[at + 9..];
        let Some(end) = inner.find("\"]") else {
            break;
        };
        if !inner.starts_with('@') {
            found.push(&inner[..end]);
        }
        rest = &inner[end + 2..];
    }
    found
}

/// `line` with each code chunk's contents cut to [`ERRORS_CHUNK_MAX_CHARS`].
fn chunks_cut(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(at) = rest.find("[string \"") {
        out.push_str(&rest[..at + 9]);
        let inner = &rest[at + 9..];
        let Some(end) = inner.find("\"]") else {
            rest = inner;
            break;
        };
        out.push_str(&cut(&inner[..end], ERRORS_CHUNK_MAX_CHARS));
        rest = &inner[end..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;
    use crate::errors::buggrabber::{EntryType, Origin};

    #[test]
    fn every_template_shape_is_recognized_with_its_text() {
        use MessageKind::*;
        for (body, expected) in [
            ("attempt to index a nil value", LuaRuntime),
            ("attempt to index a nil value (field 'x')", LuaRuntime),
            ("attempt to call a nil value (method 'SetText')", LuaRuntime),
            (
                "attempt to compare a number value (local 'count')",
                LuaRuntime,
            ),
            (
                "attempt to concatenate a table value (global 'Foo_DB')",
                LuaRuntime,
            ),
            (
                "attempt to perform arithmetic on a nil value (upvalue 'n')",
                LuaRuntime,
            ),
            (
                "attempt to get length of a nil value (field 'list')",
                LuaRuntime,
            ),
            ("attempt to index field '?' (a nil value)", LuaRuntime),
            ("attempt to call method 'Show' (a nil value)", LuaRuntime),
            ("attempt to compare number with nil", LuaRuntime),
            ("attempt to compare two table values", LuaRuntime),
            (
                "bad argument #1 to 'SetText' (string expected, got nil)",
                LuaRuntime,
            ),
            (
                "bad argument #2 to '?' (number expected, got no value)",
                LuaRuntime,
            ),
            ("bad argument #1 to 'format' (invalid option)", LuaRuntime),
            (
                "bad argument #3 to 'C_Timer.After' (value expected)",
                LuaRuntime,
            ),
            ("stack overflow", LuaRuntime),
            ("C stack overflow", LuaRuntime),
            ("'=' expected near 'x'", LuaRuntime),
            (
                "'end' expected (to close 'function' at line 3) near '<eof>'",
                LuaRuntime,
            ),
            ("unexpected symbol near '..'", LuaRuntime),
            (
                "[ADDON_ACTION_FORBIDDEN] AddOn 'Bartender4' tried to call the protected function 'CastSpellByName()'.",
                Blizzard,
            ),
            (
                "[ADDON_ACTION_BLOCKED] AddOn 'Foo' tried to call the protected function 'Frame:Show()'.",
                Blizzard,
            ),
            ("Couldn't find inherited node 'BackdropTemplate'", Blizzard),
            (
                "Couldn't find inherited node \"BackdropTemplate\"",
                Blizzard,
            ),
            (
                "Deferred XML Node object named FooFrame already exists",
                Blizzard,
            ),
        ] {
            let m = strict_message(body);
            assert_eq!(
                (m.kind, m.text.as_deref()),
                (expected, Some(body)),
                "{body}"
            );
        }
    }

    #[test]
    fn anything_else_is_custom_and_only_counted() {
        for body in [
            "attempt to index a nil value (field 'FakeChar-FakeRealm')",
            "attempt to index a nil value (field 'two words')",
            "attempt to index a nil value (field 'x') again",
            "attempt to compare number with nil and more",
            "bad argument #1 to 'SetText' (FakeChar expected)",
            "'end' expected near '\"FakeChar\"'",
            "FakeChar on FakeRealm pulled",
            "[ADDON_ACTION_FORBIDDEN] AddOn 'Fake Char' tried to call the protected function 'X()'.",
            "",
        ] {
            let m = strict_message(body);
            assert_eq!(
                (m.kind, m.text, m.chars),
                (MessageKind::Custom, None, body.chars().count()),
                "{body}"
            );
        }
        // Only the first line is the message; a second line never shows.
        let m = strict_message("stack overflow\nFakeChar was here");
        assert_eq!(m.text.as_deref(), Some("stack overflow"));
    }

    #[test]
    fn locations_are_addons_relative_and_chunks_stay_code() {
        let m = strict_message("[Interface/AddOns/Foo/core.lua]:12: attempt to call a nil value");
        assert_eq!(m.location.as_deref(), Some("Foo/core.lua:12"));
        let m = strict_message("Interface\\AddOns\\Foo\\libs\\x.lua:3: stack overflow");
        assert_eq!(m.location.as_deref(), Some("Foo/libs/x.lua:3"));
        let m = strict_message("[string \"print('FakeChar')\"]:1: attempt to call a nil value");
        assert_eq!(
            (m.location.as_deref(), m.text.as_deref()),
            (Some("[custom code]:1"), Some("attempt to call a nil value"))
        );
        let m = strict_message(
            "LUA_WARNING: [Interface/AddOns/Baz/Baz.xml]:5: Couldn't find inherited node 'X'\n|cffffd200Stack:|r\n[Interface/AddOns/Baz/Baz.lua]:20: in main chunk",
        );
        assert_eq!(
            (m.kind, m.location.as_deref()),
            (MessageKind::Blizzard, Some("Baz/Baz.xml:5"))
        );
        assert_eq!(strict_message("no location here").location, None);
    }

    #[test]
    fn strict_frames_fold_hide_and_stop() {
        let stack = [
            "[Interface/AddOns/Foo/core.lua]:12: in function 'Refresh'",
            "[C]: in function 'xpcall'",
            "[C]: ?",
            "[string \"@Interface/AddOns/Foo/b.lua\"]:7: in function <Interface/AddOns/Foo/b.lua:5>",
            "Interface\\AddOns\\Foo\\a.lua:3: in main chunk",
            "[string \"print('FakeChar')\"]:1: in function <[string \"print('FakeChar')\"]:1>",
            "(tail call): ?",
            "Foo-1.2\\core.lua:9: in function `Old'",
            "AceEvent-3 (Foo):120: in function `?'",
            "FakeChar said hello",
        ]
        .join("\n");
        assert_eq!(
            strict_frames(&stack, false),
            [
                "Foo/core.lua:12: in function 'Refresh'",
                "… (2 C frames)",
                "Foo/b.lua:7: in function <Foo/b.lua:5>",
                "Foo/a.lua:3: in main chunk",
                "[custom code]",
                "… (1 C frame)",
                "Foo-1.2/core.lua:9: in function 'Old'",
                "AceEvent-3 (Foo):120: in function '?'",
            ]
        );
        let long: String = (1..=50)
            .map(|n| format!("[Interface/AddOns/Foo/f.lua]:{n}: in function 'F'\n"))
            .collect();
        let short = strict_frames(&long, false);
        assert_eq!(short.len(), ERRORS_STACK_LINES + 1);
        assert_eq!(short.last().unwrap(), "… (44 more frames)");
        let verbose = strict_frames(&long, true);
        assert_eq!(verbose.len(), ERRORS_STACK_LINES_VERBOSE + 1);
        assert_eq!(verbose.last().unwrap(), "… (10 more frames)");
    }

    fn redactor() -> WtfPathRedactor {
        WtfPathRedactor::for_paths(
            ["Account/FAKEACCT/FakeRealm/FakeChar/SavedVariables/Foo.lua"].into_iter(),
        )
    }

    fn entry(message: &str, stack: Option<&str>, locals: Option<&str>) -> BugEntry {
        BugEntry {
            message: message.to_owned(),
            stack: stack.map(str::to_owned),
            locals: locals.map(str::to_owned),
            session: Some(1),
            time: None,
            counter: 1,
            entry_type: EntryType::Error,
            origin: Origin::Local,
        }
    }

    #[test]
    fn raw_details_strip_and_redact_what_they_can() {
        let raw = raw_details(
            &entry(
                "[Interface/AddOns/Foo/x.lua]:3: |cffff0000FakeChar|r of FakeRealm (Player-1234-0ABCDEF1) |Hplayer:FakeChar-FakeRealm|h[FakeChar]|h saw |Hitem:19019|h[Thunderfury]|h |TInterface\\Icons\\x:16|t|A atlas|a\x01\x02 done\nsecond line",
                Some(
                    "[string \"local n = 'FakeChar' -- a long chunk of custom code here\"]:1: in x\n[C]: ?",
                ),
                Some("name = \"FakeChar\"\n\tdepth = 2\x03"),
            ),
            &redactor(),
        );
        assert_eq!(
            raw.message,
            "[Interface/AddOns/Foo/x.lua]:3: <character> of <realm> (<player>) <player> saw [Thunderfury]  done"
        );
        assert_eq!(
            raw.locals.as_deref(),
            Some("name = \"<character>\"\n\tdepth = 2")
        );
        // A chunk's contents: names replaced, then cut, alike in the stack and the list.
        let [chunk] = &raw.chunks[..] else {
            unreachable!("{raw:?}");
        };
        assert_eq!(chunk.chars().count(), ERRORS_CHUNK_MAX_CHARS + 1);
        assert!(chunk.starts_with("local n = '<character>'") && chunk.ends_with('…'));
        assert_eq!(raw.stack.len(), 2);
        assert!(raw.stack[0].contains(chunk.as_str()), "{}", raw.stack[0]);
        assert!(!format!("{raw:?}").contains("FakeChar"));
    }

    #[test]
    fn raw_details_are_bounded_and_say_where_they_were_cut() {
        let long = "x".repeat(ERRORS_MESSAGE_MAX_CHARS + 10);
        let stack: String = (0..50).map(|n| format!("[C]: frame {n}\n")).collect();
        let locals = "y".repeat(ERRORS_LOCALS_MAX_CHARS + 1);
        let raw = raw_details(&entry(&long, Some(&stack), Some(&locals)), &redactor());
        assert_eq!(raw.message.chars().count(), ERRORS_MESSAGE_MAX_CHARS + 1);
        assert!(raw.message.ends_with('…'));
        assert_eq!(raw.stack.len(), ERRORS_STACK_LINES_VERBOSE);
        assert_eq!(
            raw.locals.as_ref().map(|l| l.chars().count()),
            Some(ERRORS_LOCALS_MAX_CHARS + 1)
        );
    }

    #[test]
    fn fingerprints_follow_the_normalized_message_and_location() {
        let a = fingerprint(
            &normalized("x.lua:1: attempt to call a nil value"),
            Some("Foo/x.lua:1"),
        );
        assert!(a.starts_with("e:") && a.len() == 18, "{a}");
        assert_eq!(
            a,
            fingerprint(
                &normalized("x.lua:1: attempt to call a nil value\nanother line\r"),
                Some("Foo/x.lua:1")
            )
        );
        assert_ne!(
            a,
            fingerprint(
                &normalized("x.lua:1: attempt to call a nil value"),
                Some("Foo/x.lua:2")
            )
        );
        assert_ne!(
            a,
            fingerprint(
                &normalized("x.lua:2: attempt to call a nil value"),
                Some("Foo/x.lua:1")
            )
        );
        assert_eq!(
            normalized(
                "LUA_WARNING: AceAddon-3.0-13 (Foo):5: |cffff0000odd|r\n|cffffd200Stack:|r\nx"
            ),
            "AceAddon-3.0 (Foo):5: odd"
        );
        assert_eq!(
            normalized("Lib-3.0-13x and Lib-3.0"),
            "Lib-3.0-13x and Lib-3.0"
        );
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        /// Any text: no panic, and no control character but `\n` (and `\t` in locals).
        #[test]
        fn the_renderers_never_panic_or_leak_control_characters(
            message in "(\\PC|[\\x00-\\x1f|\\[\\]\"':])*",
            stack in proptest::option::of("(\\PC|[\\x00-\\x1f|\\[\\]\"':\n])*"),
            locals in proptest::option::of("(\\PC|[\\x00-\\x1f\n\t])*"),
            verbose: bool,
        ) {
            let strict = strict_message(&message);
            for text in strict.text.iter().chain(strict.location.iter()) {
                prop_assert!(!text.chars().any(char::is_control), "{text:?}");
            }
            for line in strict_frames(stack.as_deref().unwrap_or_default(), verbose) {
                prop_assert!(!line.chars().any(char::is_control), "{line:?}");
            }
            let raw = raw_details(&entry(&message, stack.as_deref(), locals.as_deref()), &redactor());
            prop_assert!(!raw.message.chars().any(char::is_control));
            for line in raw.stack.iter().chain(&raw.chunks) {
                prop_assert!(!line.chars().any(char::is_control), "{line:?}");
            }
            if let Some(locals) = &raw.locals {
                prop_assert!(!locals.chars().any(|c| c.is_control() && c != '\n' && c != '\t'));
            }
            let _ = fingerprint(&normalized(&message), strict.location.as_deref());
        }
    }
}
