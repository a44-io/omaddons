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

/// A message's first line as strict output may show it, reading every location form any
/// client writes: an error's identity, whatever database it is in.
pub fn strict_message(raw: &str) -> StrictMessage {
    strict_message_for(raw, true)
}

/// [`strict_message`] for a database of one generation (`legacy`: r152's). A modern
/// client's locations are its code's (`Interface/…`, or cut short behind `...`): a
/// "location" an addon's own text spells (`FakeChar (FakeRealm):3:`, `[FakeChar]:3:`)
/// is never shown.
pub fn strict_message_for(raw: &str, legacy: bool) -> StrictMessage {
    let first = first_line(raw);
    let (location, body) = match location(first) {
        Some(at) => (strict_location(at, legacy), first[at.len()..].trim()),
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

/// [`strict_message_for`] on one client: a secret-value taint report ([`taint_folder`])
/// shows its text only when `installed` says the folder it names is installed there, and
/// is otherwise an addon's own message, counted. Any addon can write text in that shape,
/// and a folder's name may hold a `-` as a player's `Name-Realm` does, so its grammar
/// alone proves nothing (omad-8u49).
pub fn strict_message_on(
    raw: &str,
    legacy: bool,
    installed: &dyn Fn(&str) -> bool,
) -> StrictMessage {
    let mut message = strict_message_for(raw, legacy);
    let named = message.text.as_deref().and_then(taint_folder);
    if named.is_some_and(|folder| !installed(folder)) {
        message.kind = MessageKind::Custom;
        message.text = None;
    }
    message
}

/// A message's first line, its `LUA_WARNING: ` prefix and embedded stack left out.
fn first_line(raw: &str) -> &str {
    let raw = strip_prefix_ci(raw, "LUA_WARNING: ").unwrap_or(raw);
    let raw = raw.split_once("|cffffd200Stack:|r").map_or(raw, |(m, _)| m);
    raw.lines().next().unwrap_or_default()
}

/// A location as strict output shows it.
fn strict_location(at: &str, legacy: bool) -> Option<String> {
    let f = frame(at, legacy);
    match (f.kind, f.path, f.line) {
        (FrameKind::Path, Some(path), Some(line)) if legacy || code_path(path) => {
            Some(format!("{}:{line}", display_path(path)))
        }
        (FrameKind::Chunk, ..) => Some(match trailing_line(at) {
            Some(line) => format!("[custom code]:{line}"),
            None => "[custom code]".to_owned(),
        }),
        _ => None,
    }
}

/// Whether a modern client's location `path` names its code: under `Interface/` (an
/// addon's or the client's own), or a path cut short behind `...`.
fn code_path(path: &str) -> bool {
    let rest = path.trim_start_matches('.');
    let cut = rest.len() < path.len();
    rest.get(..10).is_some_and(|head| {
        head.eq_ignore_ascii_case("Interface/") || head.eq_ignore_ascii_case("Interface\\")
    }) || (cut && rest.contains(['/', '\\']))
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
    let blizzard: [fn(&mut Cursor<'_>) -> Option<()>; 4] = [
        // `<fn>(): <what> cannot be accessed when secret while tainted by '<folder>'`
        // ([`taint_folder`]).
        |c| {
            taint_folder(c.0)?;
            c.0 = "";
            Some(())
        },
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

/// What a secret-value taint report says cannot be read, as captures show it: a closed
/// list, so a message naming anything else stays an addon's own (omad-8u49).
const SECRET_SUBJECTS: &[&str] = &["Auras"];

/// The folder the client's own secret-value taint report names, when `body` (a
/// message's first line, without `LUA_WARNING: `) is exactly
/// `<fn>(): <what> cannot be accessed when secret while tainted by '<folder>'`: what
/// Midnight-era engines (retail 12, WoW: Forever) write when code an addon tainted reads
/// a secret value. `<fn>` is an identifier (`C_UnitAuras.GetAuraDataByIndex`), `<what>`
/// one of [`SECRET_SUBJECTS`] and `<folder>` an AddOns folder's name. As for the
/// `ADDON_ACTION` template, the name's grammar is the check: whether it is installed is
/// attribution's question.
pub(crate) fn taint_folder(body: &str) -> Option<&str> {
    let mut c = Cursor(body);
    c.word(|w| w.strip_suffix("():").is_some_and(is_identifier))?;
    c.lit(" ")?;
    c.one(SECRET_SUBJECTS)?;
    c.lit(" cannot be accessed when secret while tainted by '")?;
    let (folder, rest) = c.0.split_once('\'')?;
    (rest.is_empty() && is_folder_name(folder)).then_some(folder)
}

/// A hole that holds an AddOns folder's name: ASCII letters, digits, `_`, `-`, `.` and
/// `!`, with at least one letter or digit (`DBM-Core`, `!BugGrabber`). A space, a quote
/// or a `|` is never one.
fn is_folder_name(text: &str) -> bool {
    text.bytes().any(|b| b.is_ascii_alphanumeric())
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b'!'))
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
    format!(
        "e:{}",
        short_digest(&[normalized_message, location.unwrap_or_default()])
    )
}

/// An error's identity for "has something of this shape been seen here before" (design
/// 7.1.2): looser than [`fingerprint`], so the same bug on another line, or with other
/// numbers or names in its text, is in the same family. `a:` and 16 hex digits of the
/// SHA-256 of the [`normalized`] message as [`family_text`] reads it, and of its
/// location (normalized, as an error's identity reads it) without the line.
///
/// A family only ever suppresses a regression claim (something of this shape was
/// already known on the client): nothing may look up a verdict or a remedy by it.
/// Errors with one fingerprint always share a family. Raise
/// [`crate::limits::LEDGER_IDENTITY`] whenever this changes.
pub fn family_key(message: &str) -> String {
    family_of(&normalized(message), identity_location(message).as_deref())
}

/// An error's fingerprint and family key together, each part computed once: what the
/// errors ledger gives every entry it reads. The same as [`fingerprint`] over the
/// normalized message and location, and [`family_key`].
pub fn identities(message: &str) -> (String, String) {
    let text = normalized(message);
    let location = identity_location(message);
    (
        fingerprint(&text, location.as_deref()),
        family_of(&text, location.as_deref()),
    )
}

/// The location an error's identity reads: [`strict_message`]'s, normalized, found
/// without recognizing the message's template.
fn identity_location(message: &str) -> Option<String> {
    location(first_line(message))
        .and_then(|at| strict_location(at, true))
        .map(|l| normalized(&l))
}

/// The family key of a normalized message and its normalized location.
fn family_of(normalized_message: &str, location: Option<&str>) -> String {
    let file = location.map_or("", without_line);
    format!(
        "a:{}",
        short_digest(&[&family_text(normalized_message), file])
    )
}

/// 16 lowercase hex digits of the SHA-256 of `parts`, a zero byte between each two: the
/// opaque half of the errors' identities (`e:`, `a:`, and the errors ledger's `c:` and
/// `g:`). No part holds a zero byte.
pub(crate) fn short_digest(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for (i, part) in parts.iter().enumerate() {
        if i > 0 {
            hasher.update([0]);
        }
        hasher.update(part.as_bytes());
    }
    hasher
        .finalize()
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The fewest hexadecimal digits in a row that [`family_text`] reads as an address with
/// no `0x` (the game prints a table as `table: 000001D2C3E4F5A0`). Fewer are a word and
/// a number: `abc1234` reads `abc#`, `abc12345` reads `#`.
const ADDRESS_MIN_HEX_DIGITS: usize = 8;

/// A normalized message as a family reads it (design 7.1.2), in this order: every `0x`
/// address and every run of [`ADDRESS_MIN_HEX_DIGITS`] or more hexadecimal digits
/// becomes `#`; every remaining run of digits becomes `#`; and the text between a pair
/// of quotes, single or double, goes, the quotes staying.
fn family_text(normalized_message: &str) -> String {
    blank_quoted(&blank_digits(&blank_addresses(normalized_message)))
}

/// `text` with every `0x` address, and every run of [`ADDRESS_MIN_HEX_DIGITS`] or more
/// hexadecimal digits, as `#`.
fn blank_addresses(text: &str) -> String {
    let bytes = text.as_bytes();
    let hex_run = |from: usize| {
        bytes[from..]
            .iter()
            .take_while(|b| b.is_ascii_hexdigit())
            .count()
    };
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'0' && matches!(bytes.get(i + 1), Some(b'x' | b'X')) {
            let digits = hex_run(i + 2);
            if digits > 0 {
                out.push('#');
                i += 2 + digits;
                continue;
            }
        }
        if i == 0 || !bytes[i - 1].is_ascii_hexdigit() {
            let digits = hex_run(i);
            if digits >= ADDRESS_MIN_HEX_DIGITS {
                out.push('#');
                i += digits;
                continue;
            }
        }
        let len = text[i..].chars().next().map_or(1, char::len_utf8);
        out.push_str(&text[i..i + len]);
        i += len;
    }
    out
}

/// `text` with every run of ASCII digits as `#`.
fn blank_digits(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_digits = false;
    for c in text.chars() {
        if !c.is_ascii_digit() {
            out.push(c);
        } else if !in_digits {
            out.push('#');
        }
        in_digits = c.is_ascii_digit();
    }
    out
}

/// `text` without the text between each pair of quotes, single or double; the quotes
/// stay. A quote with no partner after it stays as it is, and the search goes on after
/// it.
fn blank_quoted(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find(['\'', '"']) {
        let quote = &rest[open..=open];
        out.push_str(&rest[..=open]);
        rest = &rest[open + 1..];
        if let Some(close) = rest.find(quote) {
            out.push_str(quote);
            rest = &rest[close + 1..];
        }
    }
    out.push_str(rest);
    out
}

/// A location without its trailing `:<line>`: `Foo/x.lua:12` is `Foo/x.lua`.
fn without_line(location: &str) -> &str {
    match location.rsplit_once(':') {
        Some((file, line)) if !line.is_empty() && line.bytes().all(|b| b.is_ascii_digit()) => file,
        _ => location,
    }
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
/// `<character>`. Best effort: a name it never learned stays. A `legacy` (r152) entry
/// keeps its frames in the message, after the first line, as strict output reads them.
pub fn raw_details(entry: &BugEntry, legacy: bool, names: &WtfPathRedactor) -> Raw {
    let safe = |text: &str, tabs: bool| names.text(&clean(&players(text), tabs));
    let message = cut(
        &safe(first_line(&entry.message), false),
        ERRORS_MESSAGE_MAX_CHARS,
    );
    let stack_text = match &entry.stack {
        Some(stack) => stack.as_str(),
        None if legacy => entry.message.split_once('\n').map_or("", |(_, rest)| rest),
        None => "",
    };
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
            // The secret-value taint report (omad-8u49), with qualified functions and
            // folder names an identifier would not take.
            (
                "GetAuraDataByIndex(): Auras cannot be accessed when secret while tainted by 'OneForAll'",
                Blizzard,
            ),
            (
                "C_UnitAuras.GetAuraDataByIndex(): Auras cannot be accessed when secret while tainted by 'DBM-Core'",
                Blizzard,
            ),
            (
                "GetAuraDataByIndex(): Auras cannot be accessed when secret while tainted by '!Foo'",
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

    /// The taint report as BugGrabber keeps it: its second line and the `LUA_WARNING:`
    /// form's embedded stack never show, and the first line is the client's message.
    #[test]
    fn a_taint_report_shows_its_first_line_and_names_its_folder() {
        let line = "GetAuraDataByIndex(): Auras cannot be accessed when secret while tainted by 'OneForAll'";
        let plain = format!("{line}\nLua Taint: OneForAll");
        let warning = format!(
            "LUA_WARNING: {line}\nLua Taint: OneForAll\n|cffffd200Stack:|r\n[C]: in function 'secureexecuterange'"
        );
        for raw in [plain.as_str(), warning.as_str()] {
            let m = strict_message(raw);
            assert_eq!(
                (m.kind, m.text.as_deref(), m.location),
                (MessageKind::Blizzard, Some(line), None),
                "{raw}"
            );
        }
        assert_eq!(taint_folder(line), Some("OneForAll"));
        // On a client, only a folder installed there makes the text the client's own.
        let installed = |folder: &str| folder == "OneForAll";
        let shown = strict_message_on(&plain, false, &installed);
        assert_eq!(
            (shown.kind, shown.text.as_deref()),
            (MessageKind::Blizzard, Some(line))
        );
        let player = "GetAuraDataByIndex(): Auras cannot be accessed when secret while tainted by 'FakeChar-FakeRealm'";
        let hidden = strict_message_on(player, false, &installed);
        assert_eq!(
            (hidden.kind, hidden.text, hidden.chars),
            (MessageKind::Custom, None, player.chars().count())
        );
        let other = strict_message_on("stack overflow", false, &|_| false);
        assert_eq!(
            other.text.as_deref(),
            Some("stack overflow"),
            "other templates as before"
        );
        assert_eq!(
            normalized(&plain),
            normalized(&warning),
            "one event in either form: one fingerprint"
        );
    }

    #[test]
    fn a_taint_look_alike_is_an_addons_own_message() {
        let head = "GetAuraDataByIndex(): Auras cannot be accessed when secret while tainted by";
        for body in [
            format!("{head} 'OneForAll' again"),
            format!("{head} 'Fake Char'"),
            format!("{head} 'Fake|cffff0000Char'"),
            format!("{head} \"OneForAll\""),
            format!("{head} ''"),
            format!("{head} '...'"),
            "GetAuraDataByIndex: Auras cannot be accessed when secret while tainted by 'OneForAll'"
                .to_owned(),
            "GetAuraDataByIndex(): Spells cannot be accessed when secret while tainted by 'OneForAll'"
                .to_owned(),
            "GetAuraDataByIndex(): Auras can be accessed when secret while tainted by 'OneForAll'"
                .to_owned(),
            "Fake Char(): Auras cannot be accessed when secret while tainted by 'OneForAll'"
                .to_owned(),
        ] {
            let m = strict_message(&body);
            assert_eq!(
                (m.kind, m.text, taint_folder(&body)),
                (MessageKind::Custom, None, None),
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

    /// On a modern client, a "location" spelled by an addon's own text is never shown:
    /// only paths under `Interface/`, or cut short behind `...`. r152's library form
    /// stays a location on its own generation.
    #[test]
    fn a_modern_location_is_its_codes_never_an_addons_text() {
        let shown = |raw: &str, legacy| strict_message_for(raw, legacy).location;
        for raw in [
            "FakeChar (FakeRealm):3: wiped",
            "[FakeChar]:3: wiped",
            "Couldn't load FakeChar from Foo.lua:12: x",
            "...FakeChar.lua:3: x",
        ] {
            assert_eq!(shown(raw, false), None, "{raw}");
        }
        assert_eq!(
            shown("Interface\\AddOns\\Foo\\x.lua:3: stack overflow", false).as_deref(),
            Some("Foo/x.lua:3")
        );
        assert_eq!(
            shown("Interface/FrameXML/ChatFrame.lua:9: stack overflow", false).as_deref(),
            Some("Interface/FrameXML/ChatFrame.lua:9")
        );
        assert_eq!(
            shown("...ace/AddOns/Foo/x.lua:3: stack overflow", false).as_deref(),
            Some("Foo/x.lua:3")
        );
        assert_eq!(
            shown(
                "[string \"@Interface/AddOns/Foo/x.lua\"]:3: stack overflow",
                false
            )
            .as_deref(),
            Some("Foo/x.lua:3")
        );
        assert_eq!(
            shown("CallbackHandler-1.0-6 (Details):146: stack overflow", true).as_deref(),
            Some("CallbackHandler-1.0-6 (Details):146")
        );
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
            false,
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

    /// r152 keeps its frames in the message after the first line, and has no stack of
    /// its own: the raw stack is those frames, as strict output's is.
    #[test]
    fn a_legacy_entrys_raw_stack_is_its_messages_frames() {
        let message = "Foo-1.2\\core.lua:12: attempt to index\nFoo-1.2\\core.lua:12: in function `Refresh'\n[C]: in function `pcall'";
        let raw = raw_details(&entry(message, None, None), true, &redactor());
        assert_eq!(raw.message, "Foo-1.2\\core.lua:12: attempt to index");
        assert_eq!(
            raw.stack,
            [
                "Foo-1.2\\core.lua:12: in function `Refresh'",
                "[C]: in function `pcall'"
            ]
        );
        assert!(
            raw_details(&entry(message, None, None), false, &redactor())
                .stack
                .is_empty()
        );
    }

    #[test]
    fn raw_details_are_bounded_and_say_where_they_were_cut() {
        let long = "x".repeat(ERRORS_MESSAGE_MAX_CHARS + 10);
        let stack: String = (0..50).map(|n| format!("[C]: frame {n}\n")).collect();
        let locals = "y".repeat(ERRORS_LOCALS_MAX_CHARS + 1);
        let raw = raw_details(
            &entry(&long, Some(&stack), Some(&locals)),
            false,
            &redactor(),
        );
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

    #[test]
    fn family_keys_join_what_only_lines_numbers_addresses_or_names_tell_apart() {
        let at = |file: &str, body: &str| format!("Interface/AddOns/{file}:12: {body}");
        let guide = "attempt to index local 'guide' (a nil value)";
        // (case, a, b, one family)
        for (case, a, b, same) in [
            (
                "the motivating message on another line",
                format!("Interface/AddOns/Guidelime/Guidelime.lua:96: {guide}"),
                format!("Interface/AddOns/Guidelime/Guidelime.lua:98: {guide}"),
                true,
            ),
            (
                "two quoted names",
                at("Foo/Foo.lua", "Unknown unit 'Thrall'"),
                at("Foo/Foo.lua", "Unknown unit 'Jaina'"),
                true,
            ),
            (
                "two double-quoted names",
                at("Foo/Foo.lua", "no frame \"FooFrame1\""),
                at("Foo/Foo.lua", "no frame \"BarFrame\""),
                true,
            ),
            (
                "two numbers",
                at("Foo/Foo.lua", "Unknown spell 12345"),
                at("Foo/Foo.lua", "Unknown spell 678"),
                true,
            ),
            (
                "two 0x addresses",
                at("Foo/Foo.lua", "bad self 0x7f3a2b10"),
                at("Foo/Foo.lua", "bad self 0xDEAD"),
                true,
            ),
            (
                "two bare addresses",
                at("Foo/Foo.lua", "bad argument #1 (table: 000001D2C3E4F5A0)"),
                at("Foo/Foo.lua", "bad argument #1 (table: 00000002AABBCCDD)"),
                true,
            ),
            (
                "in two files",
                at("Foo/Foo.lua", "attempt to call a nil value"),
                at("Foo/Bar.lua", "attempt to call a nil value"),
                false,
            ),
            (
                "in two folders that differ only in a digit",
                at("Foo1/x.lua", "attempt to call a nil value"),
                at("Foo2/x.lua", "attempt to call a nil value"),
                false,
            ),
            (
                "two message shapes in one file",
                at("Foo/Foo.lua", "attempt to call a nil value"),
                at("Foo/Foo.lua", "attempt to index a nil value"),
                false,
            ),
            (
                "seven hex digits are a word and a number, eight an address",
                at("Foo/Foo.lua", "frame abc1234 failed"),
                at("Foo/Foo.lua", "frame abc12345 failed"),
                false,
            ),
            (
                "no location, twice",
                "attempt to call a nil value".to_owned(),
                "attempt to call a nil value".to_owned(),
                true,
            ),
            (
                "no location, and a location",
                "attempt to call a nil value".to_owned(),
                at("Foo/Foo.lua", "attempt to call a nil value"),
                false,
            ),
        ] {
            let (ka, kb) = (family_key(&a), family_key(&b));
            for key in [&ka, &kb] {
                assert!(key.starts_with("a:") && key.len() == 18, "{case}: {key}");
            }
            assert_eq!(ka == kb, same, "{case}: {a:?} is {ka}, {b:?} is {kb}");
        }
    }

    #[test]
    fn a_family_reads_addresses_numbers_and_quoted_names_as_blanks() {
        for (case, text, expected) in [
            ("seven hex digits", "abc1234", "abc#"),
            ("eight hex digits", "abc12345", "#"),
            ("eight hex digits after a letter", "xabc12345", "x#"),
            ("0x addresses of any length", "0x1 0xdeadBEEF 0X0", "# # #"),
            ("0x with no digit", "0x", "#x"),
            ("a bare address", "table: 000001D2C3E4F5A0", "table: #"),
            ("numbers", "spell 12345 rank 3", "spell # rank #"),
            ("a line number", "Foo.lua:96: x", "Foo.lua:#: x"),
            (
                "quoted names, the quotes kept",
                "unit 'Thrall' and \"Jaina\"",
                "unit '' and \"\"",
            ),
            ("digits inside quotes", "'abc1234'", "''"),
            (
                "an apostrophe pairs with the next quote",
                "can't find 'X'",
                "can''X'",
            ),
            ("a quote with no partner", "it's \"x\"", "it's \"\""),
            (
                "nothing to blank",
                "attempt to call a nil value",
                "attempt to call a nil value",
            ),
            ("not ASCII", "héllo 12 ünit 'ß'", "héllo # ünit ''"),
        ] {
            assert_eq!(family_text(text), expected, "{case}: {text:?}");
        }
    }

    #[test]
    fn a_location_loses_only_its_line() {
        for (location, expected) in [
            ("Foo/x.lua:12", "Foo/x.lua"),
            ("Foo/x.lua", "Foo/x.lua"),
            ("[custom code]:4", "[custom code]"),
            ("[custom code]", "[custom code]"),
            ("Foo/x.lua:", "Foo/x.lua:"),
            ("Foo/x.lua:1a", "Foo/x.lua:1a"),
        ] {
            assert_eq!(without_line(location), expected, "{location:?}");
        }
    }

    /// One slot of a generated message: two values of one kind (a decimal number, a `0x`
    /// address, a bare address of 8 to 16 hex digits, a quoted name).
    fn slot() -> impl Strategy<Value = (String, String)> {
        prop_oneof![
            ("[0-9]{1,9}", "[0-9]{1,9}"),
            ("0x[0-9a-fA-F]{1,16}", "0x[0-9a-fA-F]{1,16}"),
            ("[0-9A-F]{8,16}", "[0-9A-F]{8,16}"),
            ("'[A-Za-z0-9 _.:-]{0,12}'", "'[A-Za-z0-9 _.:-]{0,12}'"),
            ("\"[A-Za-z0-9 _.:-]{0,12}\"", "\"[A-Za-z0-9 _.:-]{0,12}\""),
        ]
    }

    /// A message of the grammar: an AddOns path and line, then each word followed by its
    /// slot's first value (its second at `swap`).
    fn grammar_message(
        path: &str,
        line: &str,
        parts: &[(String, (String, String))],
        swap: Option<usize>,
    ) -> String {
        let mut text = format!("Interface/AddOns/{path}.lua:{line}:");
        for (i, (word, (first, second))) in parts.iter().enumerate() {
            text.push(' ');
            text.push_str(word);
            text.push(' ');
            text.push_str(if swap == Some(i) { second } else { first });
        }
        text
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        /// Another value in any one slot, or another line, keeps a message's family;
        /// another file path or another word does not.
        #[test]
        fn a_family_holds_through_its_slots_and_breaks_on_its_words(
            path in "[A-Z][A-Za-z]{1,8}(-[A-Z][a-z]{1,6})?/[A-Z][A-Za-z]{1,8}",
            other_path in "[A-Z][A-Za-z]{1,8}(-[A-Z][a-z]{1,6})?/[A-Z][A-Za-z]{1,8}",
            lines in ("[1-9][0-9]{0,4}", "[1-9][0-9]{0,4}"),
            parts in proptest::collection::vec(("[a-z]{1,7}", slot()), 1..6),
            at in any::<prop::sample::Index>(),
            other_word in "[a-z]{1,7}",
        ) {
            let i = at.index(parts.len());
            let key = family_key(&grammar_message(&path, &lines.0, &parts, None));
            prop_assert_eq!(&key, &family_key(&grammar_message(&path, &lines.0, &parts, Some(i))));
            prop_assert_eq!(&key, &family_key(&grammar_message(&path, &lines.1, &parts, None)));
            if other_path != path {
                prop_assert_ne!(&key, &family_key(&grammar_message(&other_path, &lines.0, &parts, None)));
            }
            if other_word != parts[i].0 {
                let mut changed = parts.clone();
                changed[i].0 = other_word;
                prop_assert_ne!(&key, &family_key(&grammar_message(&path, &lines.0, &changed, None)));
            }
        }
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
            let raw = raw_details(&entry(&message, stack.as_deref(), locals.as_deref()), verbose, &redactor());
            prop_assert!(!raw.message.chars().any(char::is_control));
            for line in raw.stack.iter().chain(&raw.chunks) {
                prop_assert!(!line.chars().any(char::is_control), "{line:?}");
            }
            if let Some(locals) = &raw.locals {
                prop_assert!(!locals.chars().any(|c| c.is_control() && c != '\n' && c != '\t'));
            }
            let _ = fingerprint(&normalized(&message), strict.location.as_deref());
            // The shared computation is the two identities, as `errors` makes them.
            let location = strict.location.as_deref().map(normalized);
            prop_assert_eq!(
                identities(&message),
                (fingerprint(&normalized(&message), location.as_deref()), family_key(&message))
            );
        }
    }
}
