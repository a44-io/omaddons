//! SavedVariables, read (M3-D17): a hand-written, infallible parser of the Lua literals
//! the client writes, in both writer formats (modern: no indentation, CRLF, a leading
//! blank line; 3.3.5a: tab indentation and `-- [n]` comments), with the tolerance that
//! hand-edited and older files need: bare keys, `;` separators, single quotes, long
//! strings, every escape, and the `inf`/`nan`/`1.#INF` spellings. Nothing is evaluated:
//! there is no expression beyond a number's unary minus.
//!
//! The input is untrusted (the client writes it, addons fill it, players edit it), so
//! every bound holds whatever it holds:
//! - only the globals asked for are built; every other one is scanned in skip mode,
//!   checked and bounded but allocating no value;
//! - an explicit stack replaces recursion, at most [`LUA_MAX_DEPTH`] tables deep, in
//!   both modes;
//! - at most [`LUA_MAX_NODES`] values are built, and the string bytes copied out never
//!   exceed the input's length;
//! - at most [`LUA_MAX_ISSUES`] issues are recorded, the rest only counted;
//! - the cursor never moves backwards, and resynchronization scans forward only, so
//!   the work is linear in the input (a step counter checks it in debug builds).
//!
//! Strings stay bytes ([`Value::Str`]): WoW strings are bytes, and they become text only
//! where they are rendered (M3-D21). Issues carry offsets and kinds, never content.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Read as _};
use std::path::Path;

use crate::limits::{LUA_MAX_DEPTH, LUA_MAX_ISSUES, LUA_MAX_NODES, SAVEDVARIABLES_MAX_BYTES};

/// A value as the client stores it.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Nil,
    Bool(bool),
    Int(i64),
    Num(f64),
    /// The bytes as written, escapes resolved: not text until rendered.
    Str(Vec<u8>),
    Table(Table),
}

impl Value {
    /// A string's bytes as text, invalid UTF-8 replaced; `None` for any other value.
    pub fn as_str_lossy(&self) -> Option<Cow<'_, str>> {
        match self {
            Self::Str(bytes) => Some(String::from_utf8_lossy(bytes)),
            _ => None,
        }
    }
}

/// A table: its positional fields in order, and its keyed fields in file order
/// (bracketed integer keys included, and duplicates kept: their meaning is the
/// reader's).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Table {
    pub array: Vec<Value>,
    pub hash: Vec<(Key, Value)>,
}

impl Table {
    /// The value under the string key `key`, bracketed or bare; the last one written
    /// when the key repeats.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.hash
            .iter()
            .rev()
            .find(|(k, _)| matches!(k, Key::Str(s) if s == key.as_bytes()))
            .map(|(_, v)| v)
    }

    /// The sequence from index 1: the positional fields, then on from the next index
    /// through bracketed integer keys (`[n] = …`, as hand edits and older writers
    /// write it) until an index is missing.
    pub fn seq(&self) -> impl Iterator<Item = &Value> {
        let keyed: BTreeMap<i64, &Value> = self
            .hash
            .iter()
            .filter_map(|(k, v)| k.index().map(|n| (n, v)))
            .collect();
        let next = i64::try_from(self.array.len())
            .unwrap_or(i64::MAX)
            .saturating_add(1);
        let rest = (next..i64::MAX).map_while(move |n| keyed.get(&n).copied());
        self.array.iter().chain(rest)
    }
}

/// A key.
#[derive(Debug, Clone, PartialEq)]
pub enum Key {
    Int(i64),
    Num(f64),
    Str(Vec<u8>),
    Bool(bool),
}

impl Key {
    /// The integer index this key names (`[2]`, or `[2.0]`, which Lua reads as `[2]`).
    fn index(&self) -> Option<i64> {
        match self {
            Self::Int(n) => Some(*n),
            // An integral float this small converts exactly.
            Self::Num(x) if x.fract() == 0.0 && x.abs() < 9.0e15 => Some(*x as i64),
            _ => None,
        }
    }
}

/// Something the parser met and recovered from, at a byte offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LuaIssue {
    pub offset: usize,
    pub kind: LuaIssueKind,
}

/// What a [`LuaIssue`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LuaIssueKind {
    /// A raw newline inside a string: the string ends there and reading goes on at the
    /// next line.
    UnterminatedString,
    /// An escape Lua does not know, or a decimal one past 255: kept as written.
    BadEscape,
    /// Tables nested past [`LUA_MAX_DEPTH`].
    DepthLimit,
    /// [`LUA_MAX_NODES`] values built: the rest is only scanned.
    NodeLimit,
    /// A byte that cannot start or continue what was being read.
    Unexpected(u8),
    /// The input ended inside a value (a file being written).
    TruncatedValue,
    /// More than [`SAVEDVARIABLES_MAX_BYTES`]: not parsed.
    InputTooLarge,
}

/// What a parse found.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Parsed {
    /// The globals asked for that the input assigns: the last assignment wins, and one
    /// assigned `nil` is absent.
    pub globals: BTreeMap<String, Value>,
    /// Built globals that an error or the end of the input cut: they hold what was read.
    pub partial: BTreeSet<String>,
    /// At most [`LUA_MAX_ISSUES`], in input order.
    pub issues: Vec<LuaIssue>,
    /// Issues past the cap, only counted.
    pub suppressed_issues: usize,
    /// False when the input ended inside a value, or was too large to parse.
    pub complete: bool,
}

/// A parse's work, for tests and the fuzz target.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ParseStats {
    /// Bytes passed, table-body iterations and tables closed: linear in the input.
    pub steps: u64,
    /// Values built.
    pub nodes: u64,
}

/// The most steps a parse of `len` bytes takes (M3-D17's linear-progress invariant).
#[doc(hidden)]
pub fn step_bound(len: usize) -> u64 {
    4 * len as u64 + 2 * LUA_MAX_DEPTH as u64
}

/// Parses SavedVariables `bytes`, building the globals named in `wanted` and only
/// scanning the rest. Never fails: problems are [`Parsed::issues`], and what was read
/// before them is kept.
pub fn parse_saved_variables(bytes: &[u8], wanted: &[&str]) -> Parsed {
    parse_with_stats(bytes, wanted).0
}

/// [`parse_saved_variables`], with its work counted.
#[doc(hidden)]
pub fn parse_with_stats(bytes: &[u8], wanted: &[&str]) -> (Parsed, ParseStats) {
    parse_capped(bytes, wanted, LUA_MAX_NODES)
}

fn parse_capped(bytes: &[u8], wanted: &[&str], max_nodes: u64) -> (Parsed, ParseStats) {
    let mut parsed = Parsed {
        complete: true,
        ..Parsed::default()
    };
    if bytes.len() as u64 > SAVEDVARIABLES_MAX_BYTES {
        parsed.issues.push(LuaIssue {
            offset: 0,
            kind: LuaIssueKind::InputTooLarge,
        });
        parsed.complete = false;
        return (parsed, ParseStats::default());
    }
    let mut parser = Parser {
        bytes,
        pos: 0,
        steps: 0,
        nodes: 0,
        max_nodes,
        building: false,
        exhausted: false,
        damaged: false,
        issues: Vec::new(),
        suppressed: 0,
        stack: Vec::new(),
    };
    parser.file(wanted, &mut parsed);
    debug_assert!(
        parser.steps <= step_bound(bytes.len()),
        "{} steps for {} bytes",
        parser.steps,
        bytes.len()
    );
    parsed.issues = parser.issues;
    parsed.suppressed_issues = parser.suppressed;
    let stats = ParseStats {
        steps: parser.steps,
        nodes: parser.nodes,
    };
    (parsed, stats)
}

/// [`read_bounded`]'s error payload for a file over [`SAVEDVARIABLES_MAX_BYTES`] (kind
/// [`io::ErrorKind::FileTooLarge`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("a SavedVariables file larger than {limit} bytes")]
pub struct InputTooLarge {
    pub limit: u64,
}

/// A SavedVariables file, whole, when it holds at most [`SAVEDVARIABLES_MAX_BYTES`]. A
/// larger one is an error carrying [`InputTooLarge`] (see [`is_too_large`]), with
/// nothing returned of what was read.
pub fn read_bounded(path: &Path) -> io::Result<Vec<u8>> {
    let too_large = || {
        io::Error::new(
            io::ErrorKind::FileTooLarge,
            InputTooLarge {
                limit: SAVEDVARIABLES_MAX_BYTES,
            },
        )
    };
    let file = std::fs::File::open(path)?;
    if file.metadata()?.len() > SAVEDVARIABLES_MAX_BYTES {
        return Err(too_large());
    }
    let mut bytes = Vec::new();
    // A file growing while it is read stops one byte past the cap.
    file.take(SAVEDVARIABLES_MAX_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > SAVEDVARIABLES_MAX_BYTES {
        return Err(too_large());
    }
    Ok(bytes)
}

/// Whether `err` is [`read_bounded`]'s over-the-cap error, not a failed read.
pub fn is_too_large(err: &io::Error) -> bool {
    err.get_ref().is_some_and(|e| e.is::<InputTooLarge>())
}

/// An open table.
struct Frame {
    /// Its key in its parent; `None` when positional, or for the global itself.
    key: Option<Key>,
    /// Built when the parser was building at its `{`.
    table: Option<Table>,
}

/// How reading a value stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum End {
    /// It is complete.
    Done,
    /// A structural error: resynchronize.
    Cut,
    /// The input ended inside it.
    Eof,
}

/// How reading a scalar, a key or an escape went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flow {
    Ok,
    /// Not what it should be (an issue is recorded).
    Bad,
    /// The input ended inside it.
    Eof,
}

/// A value read to its end (or as far as it went), and how it ended.
type Ended = (Option<Value>, End);

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
    steps: u64,
    nodes: u64,
    max_nodes: u64,
    /// Building the current global's values.
    building: bool,
    /// The node cap was reached: nothing more is built.
    exhausted: bool,
    /// Something cut the current global's value.
    damaged: bool,
    issues: Vec<LuaIssue>,
    suppressed: usize,
    /// The open tables, reused from global to global.
    stack: Vec<Frame>,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn peek_at(&self, ahead: usize) -> Option<u8> {
        self.bytes.get(self.pos + ahead).copied()
    }

    /// Consumes `n` bytes.
    fn bump(&mut self, n: usize) {
        self.pos = (self.pos + n).min(self.bytes.len());
        self.steps += n as u64;
    }

    fn issue(&mut self, kind: LuaIssueKind, offset: usize) {
        if self.issues.len() < LUA_MAX_ISSUES {
            self.issues.push(LuaIssue { offset, kind });
        } else {
            self.suppressed += 1;
        }
    }

    /// Counts a value about to be built; false when not building, or past the cap.
    fn node(&mut self) -> bool {
        if !self.building {
            return false;
        }
        if self.nodes >= self.max_nodes {
            self.issue(LuaIssueKind::NodeLimit, self.pos);
            self.building = false;
            self.exhausted = true;
            self.damaged = true;
            return false;
        }
        self.nodes += 1;
        true
    }

    /// The whole input: `Name = value` assignments.
    fn file(&mut self, wanted: &[&str], parsed: &mut Parsed) {
        if self.bytes.starts_with(b"\xEF\xBB\xBF") {
            self.bump(3);
        }
        loop {
            self.ws();
            let Some(b) = self.peek() else {
                return;
            };
            let start = self.pos;
            if !is_name_start(b) {
                self.issue(LuaIssueKind::Unexpected(b), start);
                self.resync(start);
                continue;
            }
            let name = String::from_utf8_lossy(self.name()).into_owned();
            self.ws();
            match self.peek() {
                Some(b'=') if self.peek_at(1) != Some(b'=') => self.bump(1),
                Some(other) => {
                    self.issue(LuaIssueKind::Unexpected(other), self.pos);
                    self.resync(start);
                    continue;
                }
                None => {
                    self.issue(LuaIssueKind::TruncatedValue, self.pos);
                    parsed.complete = false;
                    return;
                }
            }
            let asked = wanted.contains(&name.as_str());
            self.building = asked && !self.exhausted;
            // Asked for after the node cap: cut before it began.
            self.damaged = asked && self.exhausted;
            let (value, end) = self.value();
            if asked {
                let cut = end != End::Done || self.damaged;
                match value {
                    Some(Value::Nil) if !cut => {
                        parsed.globals.remove(&name);
                        parsed.partial.remove(&name);
                    }
                    Some(value) => {
                        parsed.globals.insert(name.clone(), value);
                        if cut {
                            parsed.partial.insert(name);
                        } else {
                            parsed.partial.remove(&name);
                        }
                    }
                    None if cut => {
                        parsed.partial.insert(name);
                    }
                    None => {}
                }
            }
            self.building = false;
            match end {
                End::Done => {}
                End::Cut => self.resync(start),
                End::Eof => {
                    parsed.complete = false;
                    return;
                }
            }
        }
    }

    /// Reads one value at the cursor (after a global's `=`).
    fn value(&mut self) -> Ended {
        let mut stack = std::mem::take(&mut self.stack);
        stack.clear();
        let read = self.value_with(&mut stack);
        self.stack = stack;
        read
    }

    fn value_with(&mut self, stack: &mut Vec<Frame>) -> Ended {
        if let Some(read) = self.start(stack, None) {
            return read;
        }
        // Inside a table: each pass consumes at least one byte, or ends the value.
        loop {
            self.steps += 1;
            self.ws();
            let Some(b) = self.peek() else {
                return self.eof(stack);
            };
            let read = match b {
                b',' | b';' => {
                    self.bump(1);
                    None
                }
                b'}' => {
                    self.bump(1);
                    self.close(stack)
                }
                b'[' if self.long_bracket().is_none() => self.keyed(stack),
                b if is_name_start(b) => self.named(stack),
                _ => self.start(stack, None),
            };
            if let Some(read) = read {
                return read;
            }
        }
    }

    /// Reads a value at the cursor as `key`'s (`None`: positional): opens a table, or
    /// reads a scalar into the innermost open one. `Some` when the outermost value is
    /// complete, or reading stopped.
    fn start(&mut self, stack: &mut Vec<Frame>, key: Option<Key>) -> Option<Ended> {
        self.ws();
        let Some(b) = self.peek() else {
            return Some(self.eof(stack));
        };
        if b == b'{' {
            if stack.len() >= LUA_MAX_DEPTH {
                self.issue(LuaIssueKind::DepthLimit, self.pos);
                return Some(self.unwind(stack, End::Cut));
            }
            self.bump(1);
            let table = self.node().then(Table::default);
            stack.push(Frame { key, table });
            return None;
        }
        let (value, flow) = self.scalar();
        match flow {
            Flow::Ok if stack.is_empty() => Some((value, End::Done)),
            Flow::Ok => {
                attach(stack, key, value);
                None
            }
            Flow::Bad => Some(self.unwind(stack, End::Cut)),
            // A string the input cut keeps what was read.
            Flow::Eof if stack.is_empty() => {
                self.issue(LuaIssueKind::TruncatedValue, self.pos);
                self.damaged = true;
                Some((value, End::Eof))
            }
            Flow::Eof => {
                attach(stack, key, value);
                Some(self.eof(stack))
            }
        }
    }

    /// `[key] = value`; the cursor is on the `[`.
    fn keyed(&mut self, stack: &mut Vec<Frame>) -> Option<Ended> {
        self.bump(1);
        self.ws();
        let key = match self.key() {
            (key, Flow::Ok) => key,
            (_, Flow::Bad) => return Some(self.unwind(stack, End::Cut)),
            (_, Flow::Eof) => return Some(self.eof(stack)),
        };
        for want in *b"]=" {
            self.ws();
            match self.peek() {
                Some(b) if b == want => self.bump(1),
                Some(b) => {
                    self.issue(LuaIssueKind::Unexpected(b), self.pos);
                    return Some(self.unwind(stack, End::Cut));
                }
                None => return Some(self.eof(stack)),
            }
        }
        self.start(stack, key)
    }

    /// `name = value` (a bare key), or a keyword value (`true`, `nil`, `inf`, …).
    fn named(&mut self, stack: &mut Vec<Frame>) -> Option<Ended> {
        let start = self.pos;
        let name = self.name();
        self.ws();
        if self.peek() == Some(b'=') && self.peek_at(1) != Some(b'=') {
            self.bump(1);
            let key = self.building.then(|| Key::Str(name.to_vec()));
            return self.start(stack, key);
        }
        let Some(value) = keyword(name) else {
            let first = name.first().copied().unwrap_or(0);
            self.issue(LuaIssueKind::Unexpected(first), start);
            return Some(self.unwind(stack, End::Cut));
        };
        let value = self.node().then_some(value);
        attach(stack, None, value);
        None
    }

    /// `}`: the innermost table is complete.
    fn close(&mut self, stack: &mut Vec<Frame>) -> Option<Ended> {
        let Some(frame) = stack.pop() else {
            return Some((None, End::Cut));
        };
        self.steps += 1;
        let value = frame.table.map(Value::Table);
        if stack.is_empty() {
            return Some((value, End::Done));
        }
        attach(stack, frame.key, value);
        None
    }

    /// The input ended inside a value: what was read, cut.
    fn eof(&mut self, stack: &mut Vec<Frame>) -> Ended {
        self.issue(LuaIssueKind::TruncatedValue, self.pos);
        self.unwind(stack, End::Eof)
    }

    /// Closes every open table into its parent: the outermost holds what was read.
    fn unwind(&mut self, stack: &mut Vec<Frame>, end: End) -> Ended {
        self.damaged = true;
        let mut outermost = None;
        while let Some(frame) = stack.pop() {
            self.steps += 1;
            let table = frame.table.map(Value::Table);
            match stack.last_mut() {
                Some(parent) => attach_to(parent, frame.key, table),
                None => outermost = table,
            }
        }
        (outermost, end)
    }

    /// A string, number or keyword at the cursor; built when building.
    fn scalar(&mut self) -> (Option<Value>, Flow) {
        let Some(b) = self.peek() else {
            return (None, Flow::Eof);
        };
        match b {
            b'"' | b'\'' => {
                let build = self.node();
                let (bytes, flow) = self.string(b, build);
                (bytes.map(Value::Str), flow)
            }
            b'[' => match self.long_bracket() {
                Some(level) => {
                    let build = self.node();
                    let (bytes, flow) = self.long_string(level, build);
                    (bytes.map(Value::Str), flow)
                }
                None => {
                    self.issue(LuaIssueKind::Unexpected(b), self.pos);
                    (None, Flow::Bad)
                }
            },
            b'-' | b'.' | b'0'..=b'9' => match self.number() {
                Some(value) => (self.node().then_some(value), Flow::Ok),
                None => (None, Flow::Bad),
            },
            // A name that is not a keyword stays unread, so resynchronization starts at it.
            b if is_name_start(b) => match keyword(self.peek_name()) {
                Some(value) => {
                    self.name();
                    (self.node().then_some(value), Flow::Ok)
                }
                None => {
                    self.issue(LuaIssueKind::Unexpected(b), self.pos);
                    (None, Flow::Bad)
                }
            },
            other => {
                self.issue(LuaIssueKind::Unexpected(other), self.pos);
                (None, Flow::Bad)
            }
        }
    }

    /// A bracketed key at the cursor: a string, a number or a boolean.
    fn key(&mut self) -> (Option<Key>, Flow) {
        let Some(b) = self.peek() else {
            return (None, Flow::Eof);
        };
        let build = self.building;
        match b {
            b'"' | b'\'' => {
                let (bytes, flow) = self.string(b, build);
                (bytes.map(Key::Str), flow)
            }
            b'[' if self.long_bracket().is_some() => {
                let level = self.long_bracket().unwrap_or(0);
                let (bytes, flow) = self.long_string(level, build);
                (bytes.map(Key::Str), flow)
            }
            b'-' | b'.' | b'0'..=b'9' => match self.number() {
                Some(Value::Int(n)) => (build.then_some(Key::Int(n)), Flow::Ok),
                Some(Value::Num(x)) => (build.then_some(Key::Num(x)), Flow::Ok),
                _ => (None, Flow::Bad),
            },
            b if is_name_start(b) => {
                let key = match self.peek_name() {
                    b"true" => Key::Bool(true),
                    b"false" => Key::Bool(false),
                    _ => {
                        self.issue(LuaIssueKind::Unexpected(b), self.pos);
                        return (None, Flow::Bad);
                    }
                };
                self.name();
                (build.then_some(key), Flow::Ok)
            }
            other => {
                self.issue(LuaIssueKind::Unexpected(other), self.pos);
                (None, Flow::Bad)
            }
        }
    }

    /// A quoted string; the cursor is on its opening `quote`. A raw newline ends it
    /// with [`LuaIssueKind::UnterminatedString`], and reading goes on after it.
    fn string(&mut self, quote: u8, build: bool) -> (Option<Vec<u8>>, Flow) {
        self.bump(1);
        let mut out = build.then(Vec::new);
        loop {
            let bytes = self.bytes;
            let rest = bytes.get(self.pos..).unwrap_or_default();
            let run = rest
                .iter()
                .position(|&c| c == quote || c == b'\\' || c == b'\n')
                .unwrap_or(rest.len());
            if let Some(out) = out.as_mut() {
                out.extend_from_slice(&rest[..run]);
            }
            self.bump(run);
            match self.peek() {
                None => return (out, Flow::Eof),
                Some(b'\n') => {
                    self.issue(LuaIssueKind::UnterminatedString, self.pos);
                    self.damaged = true;
                    self.bump(1);
                    return (out, Flow::Ok);
                }
                Some(b'\\') => {
                    if self.escape(out.as_mut()) == Flow::Eof {
                        return (out, Flow::Eof);
                    }
                }
                Some(_) => {
                    self.bump(1);
                    return (out, Flow::Ok);
                }
            }
        }
    }

    /// An escape; the cursor is on its backslash. One Lua does not know is kept as
    /// written, with [`LuaIssueKind::BadEscape`].
    fn escape(&mut self, mut out: Option<&mut Vec<u8>>) -> Flow {
        let bytes = self.bytes;
        let start = self.pos;
        self.bump(1);
        let Some(c) = self.peek() else {
            return Flow::Eof;
        };
        let mut push = |bytes: &[u8]| {
            if let Some(out) = out.as_mut() {
                out.extend_from_slice(bytes);
            }
        };
        let simple = match c {
            b'a' => Some(7),
            b'b' => Some(8),
            b'f' => Some(12),
            b'n' => Some(b'\n'),
            b'r' => Some(b'\r'),
            b't' => Some(b'\t'),
            b'v' => Some(11),
            b'\\' | b'"' | b'\'' => Some(c),
            _ => None,
        };
        if let Some(byte) = simple {
            push(&[byte]);
            self.bump(1);
            return Flow::Ok;
        }
        match c {
            // A backslash before a line break: the line break (any of \n, \r, \r\n, \n\r).
            b'\n' | b'\r' => {
                self.bump(1);
                if matches!(self.peek(), Some(next @ (b'\n' | b'\r')) if next != c) {
                    self.bump(1);
                }
                push(b"\n");
            }
            b'0'..=b'9' => {
                let digits = bytes[self.pos..]
                    .iter()
                    .take(3)
                    .take_while(|d| d.is_ascii_digit())
                    .count();
                let value = bytes[self.pos..self.pos + digits]
                    .iter()
                    .fold(0_u32, |n, d| n * 10 + u32::from(d - b'0'));
                match u8::try_from(value) {
                    Ok(byte) => push(&[byte]),
                    Err(_) => {
                        self.issue(LuaIssueKind::BadEscape, start);
                        push(&bytes[start..self.pos + digits]);
                    }
                }
                self.bump(digits);
            }
            b'x' => {
                // Two hex digits, as Lua reads them: `from_str_radix` alone takes a sign.
                let hex = bytes
                    .get(self.pos + 1..self.pos + 3)
                    .filter(|h| h.iter().all(u8::is_ascii_hexdigit));
                let byte = hex
                    .and_then(|h| std::str::from_utf8(h).ok())
                    .and_then(|h| u8::from_str_radix(h, 16).ok());
                match byte {
                    Some(byte) => {
                        push(&[byte]);
                        self.bump(3);
                    }
                    None => {
                        self.issue(LuaIssueKind::BadEscape, start);
                        push(b"\\x");
                        self.bump(1);
                    }
                }
            }
            // `\z`: the whitespace after it, line breaks included, is not part of the
            // string.
            b'z' => {
                self.bump(1);
                while matches!(
                    self.peek(),
                    Some(b' ' | b'\t' | b'\r' | b'\n' | b'\x0b' | b'\x0c')
                ) {
                    self.bump(1);
                }
            }
            _ => {
                self.issue(LuaIssueKind::BadEscape, start);
                push(&[b'\\', c]);
                self.bump(1);
            }
        }
        Flow::Ok
    }

    /// The level of a long bracket opening at the cursor (`[[` is 0, `[==[` is 2).
    fn long_bracket(&self) -> Option<usize> {
        let rest = self.bytes.get(self.pos..)?;
        if rest.first() != Some(&b'[') {
            return None;
        }
        let level = rest[1..].iter().take_while(|&&b| b == b'=').count();
        (rest.get(1 + level) == Some(&b'[')).then_some(level)
    }

    /// A long string (`[[…]]`, `[==[…]==]`); the cursor is on its opening bracket. Its
    /// first line break, right after the opening, is not part of it.
    fn long_string(&mut self, level: usize, build: bool) -> (Option<Vec<u8>>, Flow) {
        self.bump(level + 2);
        match (self.peek(), self.peek_at(1)) {
            (Some(b'\r'), Some(b'\n')) | (Some(b'\n'), Some(b'\r')) => self.bump(2),
            (Some(b'\r' | b'\n'), _) => self.bump(1),
            _ => {}
        }
        let content = self.pos;
        loop {
            let bytes = self.bytes;
            let rest = bytes.get(self.pos..).unwrap_or_default();
            let Some(at) = rest.iter().position(|&b| b == b']') else {
                self.bump(rest.len());
                let out = build.then(|| bytes[content..].to_vec());
                return (out, Flow::Eof);
            };
            self.bump(at);
            let close = self.pos;
            let equals = bytes[close + 1..]
                .iter()
                .take(level)
                .take_while(|&&b| b == b'=')
                .count();
            if equals == level && bytes.get(close + 1 + level) == Some(&b']') {
                self.bump(level + 2);
                return (build.then(|| bytes[content..close].to_vec()), Flow::Ok);
            }
            self.bump(1);
        }
    }

    /// A number at the cursor (`-`, a digit or `.`), with the tolerated spellings of
    /// infinity and NaN; `None`, with an issue, when it is not one.
    fn number(&mut self) -> Option<Value> {
        let bytes = self.bytes;
        let start = self.pos;
        let mut i = start;
        let negative = bytes.get(i) == Some(&b'-');
        if negative {
            i += 1;
        }
        let digits_from = |at: usize, hex: bool| {
            bytes[at..]
                .iter()
                .take_while(|b| {
                    if hex {
                        b.is_ascii_hexdigit()
                    } else {
                        b.is_ascii_digit()
                    }
                })
                .count()
        };
        let sign = if negative { -1.0 } else { 1.0 };
        let value = if bytes[i..].starts_with(b"inf") || bytes[i..].starts_with(b"nan") {
            let inf = bytes[i] == b'i';
            i += 3;
            Value::Num(if inf { sign * f64::INFINITY } else { f64::NAN })
        } else if bytes[i..].starts_with(b"0x") || bytes[i..].starts_with(b"0X") {
            let n = digits_from(i + 2, true);
            if n == 0 {
                return self.not_a_number(start);
            }
            let hex = &bytes[i + 2..i + 2 + n];
            i += 2 + n;
            let magnitude = hex.iter().fold(0.0_f64, |acc, d| {
                acc * 16.0 + f64::from(char::from(*d).to_digit(16).unwrap_or(0))
            });
            match i64::from_str_radix(std::str::from_utf8(hex).unwrap_or("x"), 16) {
                Ok(n) if negative => Value::Int(-n),
                Ok(n) => Value::Int(n),
                Err(_) => Value::Num(sign * magnitude),
            }
        } else {
            let whole = digits_from(i, false);
            i += whole;
            let mut float = false;
            let mut fraction = 0;
            if bytes.get(i) == Some(&b'.') {
                float = true;
                fraction = digits_from(i + 1, false);
                i += 1 + fraction;
            }
            if whole + fraction == 0 {
                return self.not_a_number(start);
            }
            // `1.#INF` and `1.#IND`: how some C runtimes print infinity and NaN.
            if float && bytes.get(i) == Some(&b'#') {
                let word = bytes[i + 1..]
                    .iter()
                    .take_while(|b| b.is_ascii_alphanumeric())
                    .count();
                let spelled = &bytes[i + 1..i + 1 + word];
                i += 1 + word;
                if spelled.starts_with(b"INF") {
                    Value::Num(sign * f64::INFINITY)
                } else if spelled.starts_with(b"IND") || spelled.starts_with(b"QNAN") {
                    Value::Num(f64::NAN)
                } else {
                    return self.not_a_number(start);
                }
            } else {
                if matches!(bytes.get(i), Some(b'e' | b'E')) {
                    let mut at = i + 1;
                    if matches!(bytes.get(at), Some(b'+' | b'-')) {
                        at += 1;
                    }
                    let exponent = digits_from(at, false);
                    if exponent == 0 {
                        return self.not_a_number(start);
                    }
                    float = true;
                    i = at + exponent;
                }
                let text = std::str::from_utf8(&bytes[start..i]).unwrap_or("x");
                match text.parse::<i64>() {
                    Ok(n) if !float => Value::Int(n),
                    _ => match text.parse::<f64>() {
                        Ok(x) => Value::Num(x),
                        Err(_) => return self.not_a_number(start),
                    },
                }
            }
        };
        // A number runs into nothing a name or number could continue.
        if bytes
            .get(i)
            .is_some_and(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'#'))
        {
            return self.not_a_number(start);
        }
        self.bump(i - start);
        Some(value)
    }

    fn not_a_number(&mut self, start: usize) -> Option<Value> {
        let byte = self.bytes.get(start).copied().unwrap_or(0);
        self.issue(LuaIssueKind::Unexpected(byte), start);
        None
    }

    /// A name at the cursor (which is on its first byte).
    fn name(&mut self) -> &'a [u8] {
        let name = self.peek_name();
        self.bump(name.len());
        name
    }

    /// The name at the cursor, unread.
    fn peek_name(&self) -> &'a [u8] {
        let bytes = self.bytes;
        let rest = bytes.get(self.pos..).unwrap_or_default();
        let len = rest
            .iter()
            .take_while(|b| b.is_ascii_alphanumeric() || **b == b'_')
            .count();
        &rest[..len]
    }

    /// Whitespace and comments, `-- [n]` among them.
    fn ws(&mut self) {
        loop {
            match self.peek() {
                Some(b' ' | b'\t' | b'\r' | b'\n' | b'\x0b' | b'\x0c') => self.bump(1),
                Some(b'-') if self.peek_at(1) == Some(b'-') => {
                    self.bump(2);
                    if let Some(level) = self.long_bracket() {
                        self.long_string(level, false);
                    } else {
                        let bytes = self.bytes;
                        let rest = bytes.get(self.pos..).unwrap_or_default();
                        let line = rest.iter().position(|&b| b == b'\n').unwrap_or(rest.len());
                        self.bump(line);
                    }
                }
                _ => return,
            }
        }
    }

    /// After a structural error: on to the next line, at or after the cursor and past
    /// `start` (where the cut global began), that starts with `Name =`. Forward only.
    fn resync(&mut self, start: usize) {
        let bytes = self.bytes;
        let mut at = self.pos;
        loop {
            let line_start = at == 0 || bytes.get(at - 1) == Some(&b'\n');
            if line_start && at > start && self.assignment_at(at) {
                self.pos = at;
                return;
            }
            let rest = bytes.get(at..).unwrap_or_default();
            match rest.iter().position(|&b| b == b'\n') {
                Some(newline) => {
                    self.steps += newline as u64 + 1;
                    at += newline + 1;
                }
                None => {
                    self.steps += rest.len() as u64;
                    self.pos = bytes.len();
                    return;
                }
            }
        }
    }

    /// Whether the line at `at` starts with `Name =` (no indentation).
    fn assignment_at(&mut self, at: usize) -> bool {
        let rest = self.bytes.get(at..).unwrap_or_default();
        if !rest.first().is_some_and(|b| is_name_start(*b)) {
            return false;
        }
        let name = rest
            .iter()
            .take_while(|b| b.is_ascii_alphanumeric() || **b == b'_')
            .count();
        let blank = rest[name..]
            .iter()
            .take_while(|b| matches!(b, b' ' | b'\t'))
            .count();
        self.steps += (name + blank) as u64;
        rest.get(name + blank) == Some(&b'=') && rest.get(name + blank + 1) != Some(&b'=')
    }
}

/// Adds `value` to the innermost open table, under `key` (`None`: positional).
fn attach(stack: &mut [Frame], key: Option<Key>, value: Option<Value>) {
    if let Some(frame) = stack.last_mut() {
        attach_to(frame, key, value);
    }
}

fn attach_to(frame: &mut Frame, key: Option<Key>, value: Option<Value>) {
    let (Some(table), Some(value)) = (frame.table.as_mut(), value) else {
        return;
    };
    match key {
        Some(key) => table.hash.push((key, value)),
        None => table.array.push(value),
    }
}

fn is_name_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

/// The value a keyword names.
fn keyword(name: &[u8]) -> Option<Value> {
    match name {
        b"true" => Some(Value::Bool(true)),
        b"false" => Some(Value::Bool(false)),
        b"nil" => Some(Value::Nil),
        b"inf" => Some(Value::Num(f64::INFINITY)),
        b"nan" => Some(Value::Num(f64::NAN)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `text` with every global built.
    fn parse(text: &[u8]) -> Parsed {
        let (parsed, stats) = parse_with_stats(text, &["T", "A", "B", "C"]);
        assert!(stats.steps <= step_bound(text.len()));
        parsed
    }

    fn global<'p>(parsed: &'p Parsed, name: &str) -> &'p Value {
        parsed.globals.get(name).unwrap()
    }

    fn table(value: &Value) -> &Table {
        let Value::Table(t) = value else {
            unreachable!("not a table: {value:?}");
        };
        t
    }

    fn s(text: &[u8]) -> Value {
        Value::Str(text.to_vec())
    }

    fn kinds(parsed: &Parsed) -> Vec<LuaIssueKind> {
        parsed.issues.iter().map(|i| i.kind).collect()
    }

    #[test]
    fn both_writer_formats_give_the_same_tree() {
        let modern = b"\r\nT = {\r\n[\"session\"] = 3,\r\n[\"errors\"] = {\r\n{\r\n[\"message\"] = \"x:1: boom\",\r\n[\"counter\"] = 2,\r\n},\r\n\"tail\",\r\n},\r\n}\r\n";
        let legacy = b"\r\nT = {\r\n\t[\"session\"] = 3,\r\n\t[\"errors\"] = {\r\n\t\t{\r\n\t\t\t[\"message\"] = \"x:1: boom\",\r\n\t\t\t[\"counter\"] = 2,\r\n\t\t}, -- [1]\r\n\t\t\"tail\", -- [2]\r\n\t},\r\n}\r\n";
        let (m, l) = (parse(modern), parse(legacy));
        assert!(m.issues.is_empty() && m.complete, "{m:?}");
        assert_eq!(m, l);
        let t = table(global(&m, "T"));
        assert_eq!(t.get("session"), Some(&Value::Int(3)));
        let errors = table(t.get("errors").unwrap());
        assert_eq!(errors.array.len(), 2);
        assert_eq!(
            table(&errors.array[0]).get("message"),
            Some(&s(b"x:1: boom"))
        );
    }

    #[test]
    fn raw_control_bytes_and_invalid_utf8_are_kept() {
        let p = parse(b"T = \"a\x01\x02\x03\xff\xfe\rb\"");
        assert_eq!(global(&p, "T"), &s(b"a\x01\x02\x03\xff\xfe\rb"));
        assert!(p.issues.is_empty());
        assert_eq!(
            global(&p, "T").as_str_lossy().unwrap(),
            "a\x01\x02\x03\u{fffd}\u{fffd}\rb"
        );
        assert_eq!(Value::Int(1).as_str_lossy(), None);
    }

    #[test]
    fn comments_are_whitespace() {
        let p = parse(b"-- a header\nT = { -- [x\n 1, -- [1]\n --[[ a long\n comment ]] 2, --[==[ ]] ]==] 3 }\n-- trailing");
        assert_eq!(
            table(global(&p, "T")).array,
            [Value::Int(1), Value::Int(2), Value::Int(3)]
        );
        assert!(p.issues.is_empty() && p.complete);
    }

    #[test]
    fn nil_positional_and_at_the_top_level() {
        let p = parse(b"T = { nil, 1, [\"k\"] = nil }\nA = 1\nA = nil\nB = nil");
        let t = table(global(&p, "T"));
        assert_eq!(t.array, [Value::Nil, Value::Int(1)]);
        assert_eq!(t.hash, [(Key::Str(b"k".to_vec()), Value::Nil)]);
        assert!(!p.globals.contains_key("A") && !p.globals.contains_key("B"));
        assert!(p.issues.is_empty());
    }

    #[test]
    fn line_endings_a_missing_final_newline_and_a_bom() {
        for text in [
            &b"\r\nT = {\r\n1,\r\n}\r\n"[..],
            b"T = {\n1,\n}",
            b"\xEF\xBB\xBFT = { 1 }",
            b"T={1}",
        ] {
            let p = parse(text);
            assert_eq!(table(global(&p, "T")).array, [Value::Int(1)], "{text:?}");
            assert!(p.issues.is_empty() && p.complete, "{text:?}: {p:?}");
        }
    }

    #[test]
    fn numbers_and_every_spelling_of_infinity_and_nan() {
        let p = parse(
            b"T = { 0, -7, 1.5, -.5, 5., 1e3, 2E-2, 0x1F, -0x10, inf, -inf, nan, 1.#INF, -1.#INF, 1.#IND, 9223372036854775807, 9223372036854775808, -9223372036854775808 }",
        );
        assert!(p.issues.is_empty(), "{p:?}");
        let a = &table(global(&p, "T")).array;
        let expected_ints = [
            (0, 0),
            (1, -7),
            (7, 31),
            (8, -16),
            (15, i64::MAX),
            (17, i64::MIN),
        ];
        for (at, n) in expected_ints {
            assert_eq!(a[at], Value::Int(n), "{at}");
        }
        let nums = [
            (2, 1.5),
            (3, -0.5),
            (4, 5.0),
            (5, 1000.0),
            (6, 0.02),
            (9, f64::INFINITY),
            (10, f64::NEG_INFINITY),
            (12, f64::INFINITY),
            (13, f64::NEG_INFINITY),
            (16, 9.223_372_036_854_776e18),
        ];
        for (at, x) in nums {
            assert_eq!(a[at], Value::Num(x), "{at}");
        }
        for at in [11, 14] {
            assert!(matches!(a[at], Value::Num(x) if x.is_nan()), "{at}");
        }
    }

    #[test]
    fn bad_numbers_are_unexpected() {
        for text in [
            &b"T = 12abc"[..],
            b"T = 1e",
            b"T = 0x",
            b"T = -",
            b"T = 1.2.3",
            b"T = 1.#XYZ",
        ] {
            let p = parse(text);
            assert!(
                matches!(kinds(&p)[..], [LuaIssueKind::Unexpected(_)]),
                "{text:?}: {p:?}"
            );
            assert!(!p.globals.contains_key("T"), "{text:?}");
        }
    }

    #[test]
    fn bare_keys_semicolons_and_every_key_form() {
        let p = parse(b"T = { a = 1; b = 2, [\"c\"] = 3; ['d'] = 4, [5] = 5, [-1] = 6, [1.5] = 7, [true] = 8, [ [[e]] ] = 9 }");
        assert!(p.issues.is_empty(), "{p:?}");
        let t = table(global(&p, "T"));
        let keys: Vec<&Key> = t.hash.iter().map(|(k, _)| k).collect();
        assert_eq!(
            keys,
            [
                &Key::Str(b"a".to_vec()),
                &Key::Str(b"b".to_vec()),
                &Key::Str(b"c".to_vec()),
                &Key::Str(b"d".to_vec()),
                &Key::Int(5),
                &Key::Int(-1),
                &Key::Num(1.5),
                &Key::Bool(true),
                &Key::Str(b"e".to_vec()),
            ]
        );
        assert_eq!(t.get("a"), Some(&Value::Int(1)));
        assert_eq!(t.get("c"), Some(&Value::Int(3)));
    }

    #[test]
    fn long_strings() {
        let p = parse(b"T = { [[plain]], [==[a]]b]=]c]==], [[\nfirst line break dropped]], [[\r\nso is CRLF]] }");
        assert!(p.issues.is_empty(), "{p:?}");
        assert_eq!(
            table(global(&p, "T")).array,
            [
                s(b"plain"),
                s(b"a]]b]=]c"),
                s(b"first line break dropped"),
                s(b"so is CRLF")
            ]
        );
    }

    #[test]
    fn every_escape_form() {
        let p = parse(
            b"T = { \"\\a\\b\\f\\n\\r\\t\\v\\\\\\\"\\'\", '\\65\\066\\0671\\0', \"\\x41\\x7a\", \"a\\z  \n  b\", \"line\\\nnext\", \"crlf\\\r\nnext\", 'it''s' }",
        );
        let a = &table(global(&p, "T")).array;
        assert_eq!(a[0], s(b"\x07\x08\x0c\n\r\t\x0b\\\"'"));
        assert_eq!(a[1], s(b"ABC1\0"));
        assert_eq!(a[2], s(b"Az"));
        assert_eq!(a[3], s(b"ab"));
        assert_eq!(a[4], s(b"line\nnext"));
        assert_eq!(a[5], s(b"crlf\nnext"));
        // Two strings side by side, without a separator.
        assert_eq!(a[6..], [s(b"it"), s(b"s")]);
        assert!(p.issues.is_empty(), "{p:?}");
    }

    #[test]
    fn bad_escapes_are_kept_as_written() {
        let p = parse(b"T = { \"\\q\", \"\\300\", \"\\xZZ\", \"\\x+f\" }");
        assert_eq!(
            table(global(&p, "T")).array,
            [s(b"\\q"), s(b"\\300"), s(b"\\xZZ"), s(b"\\x+f")]
        );
        assert_eq!(kinds(&p), [LuaIssueKind::BadEscape; 4]);
        assert!(p.partial.is_empty(), "an escape cuts nothing");
    }

    #[test]
    fn a_raw_newline_ends_the_string_and_reading_goes_on_at_the_next_line() {
        let text = b"T = {\n\"abc\n\"def\",\n}\nB = 2\n";
        let p = parse(text);
        assert_eq!(table(global(&p, "T")).array, [s(b"abc"), s(b"def")]);
        assert_eq!(
            p.issues,
            [LuaIssue {
                offset: 10,
                kind: LuaIssueKind::UnterminatedString
            }]
        );
        assert!(p.partial.contains("T") && p.complete);
        assert_eq!(global(&p, "B"), &Value::Int(2));
    }

    #[test]
    fn the_input_ending_inside_a_value_keeps_what_was_read() {
        let p = parse(b"A = 1\nT = { 1, { \"abc");
        assert!(!p.complete);
        assert_eq!(global(&p, "A"), &Value::Int(1));
        let t = table(global(&p, "T"));
        assert_eq!(t.array[0], Value::Int(1));
        assert_eq!(table(&t.array[1]).array, [s(b"abc")]);
        assert_eq!(p.partial.iter().collect::<Vec<_>>(), ["T"]);
        assert_eq!(kinds(&p), [LuaIssueKind::TruncatedValue]);
        let p = parse(b"T = \"abc");
        assert_eq!((global(&p, "T"), p.complete), (&s(b"abc"), false));
        let p = parse(b"T =");
        assert!(!p.complete && p.partial.contains("T") && !p.globals.contains_key("T"));
    }

    #[test]
    fn a_structural_error_resynchronizes_at_the_next_assignment() {
        let p = parse(b"A = { 1, [ x ] = 2, 3 }\n  B = 9\nB = 2\n");
        assert_eq!(table(global(&p, "A")).array, [Value::Int(1)]);
        assert!(p.partial.contains("A"));
        assert_eq!(
            global(&p, "B"),
            &Value::Int(2),
            "an indented line is not a global"
        );
        assert_eq!(
            p.issues,
            [LuaIssue {
                offset: 11,
                kind: LuaIssueKind::Unexpected(b'x')
            }]
        );
        let p = parse(b"!!\nA = 1 2\nB = 3");
        assert_eq!(
            kinds(&p),
            [
                LuaIssueKind::Unexpected(b'!'),
                LuaIssueKind::Unexpected(b'2')
            ]
        );
        assert_eq!(
            (global(&p, "A"), global(&p, "B")),
            (&Value::Int(1), &Value::Int(3))
        );
    }

    #[test]
    fn the_depth_bound_holds_building_and_skipping() {
        let deep = |n: usize| format!("T = {}{}\nB = 1", "{".repeat(n), "}".repeat(n));
        let p = parse(deep(LUA_MAX_DEPTH).as_bytes());
        assert!(p.issues.is_empty(), "{} tables deep is fine", LUA_MAX_DEPTH);
        for wanted in [&["T", "B"][..], &["B"][..]] {
            let (p, stats) = parse_with_stats(deep(LUA_MAX_DEPTH + 1).as_bytes(), wanted);
            assert_eq!(kinds(&p), [LuaIssueKind::DepthLimit], "{wanted:?}");
            assert_eq!(global(&p, "B"), &Value::Int(1));
            if wanted.len() == 1 {
                assert_eq!(stats.nodes, 1, "skip mode builds nothing");
            } else {
                assert!(p.partial.contains("T"));
            }
        }
    }

    #[test]
    fn skip_mode_builds_nothing() {
        let text = b"A = { 1, \"two\", { [\"three\"] = 3 }, x = [[four]] }\nB = 1\nC = { 5 }";
        let (p, stats) = parse_with_stats(text, &["B"]);
        assert_eq!((stats.nodes, p.globals.len()), (1, 1));
        let (p, stats) = parse_with_stats(text, &[]);
        assert_eq!((stats.nodes, p.globals.len()), (0, 0));
        assert!(p.issues.is_empty() && p.complete);
        let (_, all) = parse_with_stats(text, &["A", "B", "C"]);
        // A's table and its 5 values, B's 1, C's table and its 1.
        assert_eq!(all.nodes, 9);
    }

    #[test]
    fn the_node_bound_cuts_the_global_and_skips_the_rest() {
        let (p, stats) = parse_capped(b"A = { 1, 2, 3 }\nB = 4\nC = 5", &["A", "B", "C"], 3);
        assert_eq!(stats.nodes, 3);
        assert_eq!(table(global(&p, "A")).array, [Value::Int(1), Value::Int(2)]);
        assert_eq!(kinds(&p), [LuaIssueKind::NodeLimit]);
        let partial: Vec<&String> = p.partial.iter().collect();
        assert_eq!(partial, ["A", "B", "C"]);
        assert!(p.complete, "scanned to the end");
    }

    #[test]
    fn issues_past_the_cap_are_only_counted_and_the_work_stays_linear() {
        let text = "A = {[\n".repeat(10_000);
        let (p, stats) = parse_with_stats(text.as_bytes(), &["A"]);
        assert_eq!(
            (p.issues.len(), p.suppressed_issues),
            (LUA_MAX_ISSUES, 9_936)
        );
        assert!(!p.complete, "the last fragment ends the input");
        assert!(stats.steps <= step_bound(text.len()), "{stats:?}");
        assert_eq!(p.partial.iter().collect::<Vec<_>>(), ["A"]);
    }

    #[test]
    fn adversarial_inputs_stay_within_the_step_bound() {
        let inputs: Vec<Vec<u8>> = vec![
            "{".repeat(10_000).into_bytes(),
            format!("T = {}", "{".repeat(10_000)).into_bytes(),
            // Near-miss closers of an unterminated long string.
            format!("T = [==[{}", "]=]".repeat(5_000)).into_bytes(),
            format!("T = [====[{}", "]===".repeat(5_000)).into_bytes(),
            "T = \"\\".repeat(5_000).into_bytes(),
            "T = { a a a a a a\n".repeat(2_000).into_bytes(),
            "--[[".repeat(5_000).into_bytes(),
            "A = 1\n".repeat(5_000).into_bytes(),
            "\nA\n=\n".repeat(5_000).into_bytes(),
            vec![b'='; 10_000],
        ];
        for input in inputs {
            for wanted in [&["T", "A"][..], &[][..]] {
                let (p, stats) = parse_with_stats(&input, wanted);
                assert!(stats.steps <= step_bound(input.len()), "{stats:?}");
                assert!(p.issues.len() <= LUA_MAX_ISSUES);
            }
        }
    }

    #[test]
    fn input_over_the_cap_is_not_parsed() {
        let too_big = vec![b' '; usize::try_from(SAVEDVARIABLES_MAX_BYTES).unwrap() + 1];
        let p = parse_saved_variables(&too_big, &["T"]);
        assert_eq!(kinds(&p), [LuaIssueKind::InputTooLarge]);
        assert!(!p.complete);
    }

    #[test]
    fn get_and_seq() {
        let p = parse(b"T = { \"a\", \"b\", [3] = \"c\", [5] = \"e\", x = 1, x = 2, [4.0] = \"d\" }\nA = { [1] = \"x\", [2] = \"y\", [4] = \"z\" }");
        let t = table(global(&p, "T"));
        let seq: Vec<&Value> = t.seq().collect();
        assert_eq!(seq, [&s(b"a"), &s(b"b"), &s(b"c"), &s(b"d"), &s(b"e")]);
        assert_eq!(t.get("x"), Some(&Value::Int(2)), "the last written");
        assert_eq!(t.get("y"), None);
        let a = table(global(&p, "A"));
        assert_eq!(a.seq().collect::<Vec<_>>(), [&s(b"x"), &s(b"y")]);
    }

    #[test]
    fn the_last_assignment_wins() {
        let p = parse(b"A = 1\nA = { 2 }\nB = { 1 }\nB = 2");
        assert_eq!(table(global(&p, "A")).array, [Value::Int(2)]);
        assert_eq!(global(&p, "B"), &Value::Int(2));
    }
}
