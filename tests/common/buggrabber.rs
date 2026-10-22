//! Synthetic BugGrabber SavedVariables (`BugGrabberDB`) for the Lua-errors tests, built
//! to the public structure (the 2022 and 2026 samples, BugGrabber's and BugSack's
//! source) because no real capture may be used: there is none on the development
//! machine, and real ones carry other players' names. Every BugGrabber era, both writer
//! formats ([`Writer`]), and the privacy and attribution traps later tasks must handle.
//!
//! The scenarios are a contract: the errors beads cite them by name, so add new ones
//! rather than change these. Names are the fake ones of [`super::privacy`]. Builders are
//! deterministic: times are fixed instants, never the clock.

use std::fs;
use std::path::Path;

pub use super::lua::Writer;
use super::lua::{self, Table, Value};
use super::privacy::SENDER;

/// The file BugGrabber's account-wide SavedVariables live in.
pub const FILE: &str = "!BugGrabber.lua";
/// The most bytes r152 puts in one `message` chunk.
pub const CHUNK_BYTES: usize = 980;
/// The most chunks r152 splits a `message` into.
pub const MAX_CHUNKS: usize = 7;
/// The instant the scenarios' times are relative to: 2026-10-15 18:30:00 UTC.
pub const NOW: i64 = 1_792_089_000;
const HOUR: i64 = 3600;
const DAY: i64 = 24 * HOUR;

/// How an entry's `time` is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeKind {
    /// A unix integer (v12.0.11 and later: the last occurrence).
    Unix,
    /// A local-time `YYYY/MM/DD HH:MM:SS` string (v10.1 to v12.0.9, and r152), rendered
    /// in UTC civil time so the builders do not depend on the host's zone.
    LocalString,
}

/// One entry of `BugGrabberDB.errors`.
#[derive(Debug, Clone)]
pub struct BugEntry {
    pub message: String,
    pub stack: Option<String>,
    pub locals: Option<String>,
    pub session: u32,
    /// How `time` is written, and the unix instant it holds.
    pub time: (TimeKind, i64),
    pub counter: u32,
    /// r152's `type`: `error` or `event`.
    pub entry_type: Option<&'static str>,
    /// r152: `message` as an array of chunks of at most [`CHUNK_BYTES`].
    pub chunked: bool,
    /// BugSack's received reports: the player who sent it.
    pub source: Option<&'static str>,
}

impl BugEntry {
    /// An error seen once in `session`, at `time`.
    pub fn new(message: &str, session: u32, time: (TimeKind, i64)) -> Self {
        Self {
            message: message.to_owned(),
            stack: None,
            locals: None,
            session,
            time,
            counter: 1,
            entry_type: None,
            chunked: false,
            source: None,
        }
    }

    pub fn stack(mut self, frames: &[String]) -> Self {
        self.stack = Some(frames.join("\n"));
        self
    }

    pub fn locals(mut self, locals: &str) -> Self {
        self.locals = Some(locals.to_owned());
        self
    }

    pub fn counter(mut self, counter: u32) -> Self {
        self.counter = counter;
        self
    }

    pub fn source(mut self, sender: &'static str) -> Self {
        self.source = Some(sender);
        self
    }

    /// An r152 entry of `entry_type`, whose `message` embeds `frames` (its lines after
    /// the first) and `locals` (after `"\nLocals:|r\n"`), ending `"\n  ---"`; `chunked`
    /// splits it as r152 does past [`CHUNK_BYTES`].
    pub fn r152(
        message: &str,
        frames: &[String],
        locals: &str,
        entry_type: &'static str,
        session: u32,
        time: i64,
    ) -> Self {
        let mut text = message.to_owned();
        for frame in frames {
            text.push('\n');
            text.push_str(frame);
        }
        text.push_str("\nLocals:|r\n");
        text.push_str(locals);
        text.push_str("\n  ---");
        Self {
            chunked: text.len() > CHUNK_BYTES,
            entry_type: Some(entry_type),
            ..Self::new(&text, session, (TimeKind::LocalString, time))
        }
    }

    /// The entry as BugGrabber stores it.
    pub fn value(&self) -> Value {
        let message = if self.chunked {
            let chunks = self.message.as_bytes().chunks(CHUNK_BYTES);
            assert!(chunks.len() <= MAX_CHUNKS, "r152 keeps at most 7 chunks");
            Value::Table(chunks.fold(Table::default(), |t, chunk| {
                t.push(Value::Str(chunk.to_vec()))
            }))
        } else {
            Value::str(&self.message)
        };
        let mut t = Table::default().with("message", message);
        if let Some(stack) = &self.stack {
            t = t.with("stack", Value::str(stack));
        }
        if let Some(locals) = &self.locals {
            t = t.with("locals", Value::str(locals));
        }
        if let Some(kind) = self.entry_type {
            t = t.with("type", Value::str(kind));
        }
        t = t
            .with("session", Value::Int(self.session.into()))
            .with("time", time_value(self.time))
            .with("counter", Value::Int(self.counter.into()));
        if let Some(sender) = self.source {
            t = t.with("source", Value::str(sender));
        }
        Value::Table(t)
    }
}

/// A `time` field's value.
fn time_value((kind, at): (TimeKind, i64)) -> Value {
    match kind {
        TimeKind::Unix => Value::Int(at),
        TimeKind::LocalString => {
            let civil = jiff::Timestamp::from_second(at)
                .unwrap()
                .to_zoned(jiff::tz::TimeZone::UTC);
            Value::str(&civil.strftime("%Y/%m/%d %H:%M:%S").to_string())
        }
    }
}

/// One `BugGrabberDB`.
#[derive(Debug, Clone)]
pub struct BugDb {
    /// The current session: +1 per UI load.
    pub session: u32,
    /// Oldest to newest, as BugGrabber keeps them.
    pub entries: Vec<BugEntry>,
    /// Values written after the entries: a malformed tail.
    pub tail: Vec<Value>,
    /// `limit`, `save` and `throttle`, which r152 and older DBs carry.
    pub legacy_keys: bool,
    /// r152 has no `lastSanitation` or `stopnag`.
    pub r152: bool,
    pub writer: Writer,
}

impl BugDb {
    pub fn new(writer: Writer, session: u32) -> Self {
        Self {
            session,
            entries: Vec::new(),
            tail: Vec::new(),
            legacy_keys: false,
            r152: false,
            writer,
        }
    }

    pub fn entry(mut self, entry: BugEntry) -> Self {
        self.entries.push(entry);
        self
    }

    pub fn legacy_keys(mut self) -> Self {
        self.legacy_keys = true;
        self
    }

    /// The table, for a file that holds other globals too.
    pub fn value(&self) -> Value {
        let mut errors = Table::default();
        for entry in &self.entries {
            errors = errors.push(entry.value());
        }
        for value in &self.tail {
            errors = errors.push(value.clone());
        }
        let mut t = Table::default().with("session", Value::Int(self.session.into()));
        if !self.r152 {
            t = t
                .with("lastSanitation", Value::Int(3))
                .with("stopnag", Value::Int(120_001));
        }
        t = t.with("errors", Value::Table(errors));
        if self.legacy_keys {
            t = t
                .with("limit", Value::Int(50))
                .with("save", Value::Bool(true))
                .with("throttle", Value::Bool(true));
        }
        Value::Table(t)
    }

    /// `!BugGrabber.lua`: `BugGrabberDB` alone.
    pub fn render(&self) -> Vec<u8> {
        lua::render(self.writer, &[("BugGrabberDB", self.value())])
    }

    /// Writes [`Self::render`] to `<sv_dir>/<file>`.
    pub fn write(&self, sv_dir: &Path, file: &str) {
        fs::create_dir_all(sv_dir).unwrap();
        fs::write(sv_dir.join(file), self.render()).unwrap();
    }

    /// The account-wide file under `FAKEACCT`.
    pub fn file(&self) -> SvFile {
        SvFile::account("FAKEACCT", FILE, self.render())
    }
}

/// One SavedVariables file, by its path relative to `WTF/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SvFile {
    pub rel: String,
    pub bytes: Vec<u8>,
}

impl SvFile {
    /// `Account/<account>/SavedVariables/<file>`.
    pub fn account(account: &str, file: &str, bytes: Vec<u8>) -> Self {
        Self {
            rel: format!("Account/{account}/SavedVariables/{file}"),
            bytes,
        }
    }
}

/// Writes `files` under `wtf` (a client's `WTF/`).
pub fn write_files(wtf: &Path, files: &[SvFile]) {
    for file in files {
        let path = wtf.join(&file.rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, &file.bytes).unwrap();
    }
}

// ---------------------------------------------------------------------------------------
// Stack frame locations, by era.

/// Era A (3.3.5a and old retail): `Interface\AddOns\X\f.lua:12:`.
pub fn stack_era_a(folder: &str, file: &str, line: u32) -> String {
    format!(
        "Interface\\AddOns\\{folder}\\{}:{line}:",
        file.replace('/', "\\")
    )
}

/// Era B (about 2020 to 2025): `[string "@Interface/AddOns/X/f.lua"]:12:`.
pub fn stack_era_b(folder: &str, file: &str, line: u32) -> String {
    format!("[string \"@Interface/AddOns/{folder}/{file}\"]:{line}:")
}

/// Era C (retail 11.1.5+, current Classic clients): `[Interface/AddOns/X/f.lua]:12:`.
pub fn stack_era_c(folder: &str, file: &str, line: u32) -> String {
    format!("[Interface/AddOns/{folder}/{file}]:{line}:")
}

/// r152, an addon: `Folder-version\f.lua:12:` (no `Interface\AddOns\`).
pub fn stack_r152_addon(folder: &str, version: &str, file: &str, line: u32) -> String {
    format!("{folder}-{version}\\{}:{line}:", file.replace('/', "\\"))
}

/// r152, a library: `Major-Minor (Folder):12:`.
pub fn stack_r152_library(major: &str, minor: u32, folder: &str, line: u32) -> String {
    format!("{major}-{minor} ({folder}):{line}:")
}

/// A message's location prefix as the client truncates it: past 57 characters, its end
/// behind a leading `...`.
pub fn truncated(location: &str) -> String {
    const MAX: usize = 57;
    let chars: Vec<char> = location.chars().collect();
    if chars.len() <= MAX {
        return location.to_owned();
    }
    let tail: String = chars[chars.len() - (MAX - 3)..].iter().collect();
    format!("...{tail}")
}

fn frame(location: String, function: &str) -> String {
    format!("{location} in function {function}")
}

// ---------------------------------------------------------------------------------------
// The scenarios.

/// Every scenario by name, in `writer`'s format (`flood` at 500 entries, the upstream
/// cap).
pub fn scenarios(writer: Writer) -> Vec<(&'static str, Vec<SvFile>)> {
    vec![
        ("modern_int", vec![modern_int(writer).file()]),
        (
            "modern_string_first_seen",
            vec![modern_string_first_seen(writer).file()],
        ),
        (
            "modern_string_last_seen",
            vec![modern_string_last_seen(writer).file()],
        ),
        ("r152_chunked", vec![r152_chunked(writer).file()]),
        ("embedded_bdui", embedded_bdui(writer)),
        ("warnings", vec![warnings(writer).file()]),
        ("forbidden", vec![forbidden(writer).file()]),
        ("personal", vec![personal(writer).file()]),
        ("received", vec![received(writer).file()]),
        ("custom_names", vec![custom_names(writer).file()]),
        ("two_accounts", two_accounts(writer)),
        ("mixed_times", vec![mixed_times(writer).file()]),
        ("flood", vec![flood(writer, 500).file()]),
    ]
}

/// v12.0.11+: unix integer times of the last occurrence. A plain error, a library that
/// dispatched another addon's callback (the frame below the library is the addon to
/// blame), a WeakAuras custom-code chunk, and a message whose location the client
/// truncated.
pub fn modern_int(writer: Writer) -> BugDb {
    let unix = |at| (TimeKind::Unix, at);
    let plain = BugEntry::new(
        &format!(
            "{} attempt to index a nil value (field 'db')",
            stack_era_c("Foo", "core.lua", 12)
        ),
        410,
        unix(NOW - 2 * DAY),
    )
    .stack(&[
        frame(stack_era_c("Foo", "core.lua", 12), "'Refresh'"),
        frame(
            stack_era_c("Foo", "core.lua", 40),
            "<Interface/AddOns/Foo/core.lua:38>",
        ),
    ])
    .locals("self = <table> {\n}\n(*temporary) = nil")
    .counter(3);
    let library = "Libs/CallbackHandler-1.0/CallbackHandler-1.0.lua";
    let dispatch = BugEntry::new(
        &format!(
            "{} attempt to compare number with nil",
            stack_era_c("Questie", "Modules/QuestieEvent.lua", 88)
        ),
        411,
        unix(NOW - DAY),
    )
    .stack(&[
        frame(
            stack_era_c("Questie", "Modules/QuestieEvent.lua", 88),
            "<Interface/AddOns/Questie/Modules/QuestieEvent.lua:80>",
        ),
        "[C]: in function 'xpcall'".to_owned(),
        frame(stack_era_c("AtlasLootClassic", library, 109), "'Fire'"),
        frame(
            stack_era_c(
                "AtlasLootClassic",
                "Libs/AceEvent-3.0/AceEvent-3.0.lua",
                120,
            ),
            "<Interface/AddOns/AtlasLootClassic/Libs/AceEvent-3.0/AceEvent-3.0.lua:119>",
        ),
    ]);
    let aura = BugEntry::new(
        "[string \"--[==[ Error in 'Interrupt Bar' ]==] return function(...)\"]:4: attempt to perform arithmetic on a nil value",
        412,
        unix(NOW - HOUR),
    )
    .stack(&[
        "[string \"--[==[ Error in 'Interrupt Bar' ]==] return function(...)\"]:4: in function <[string \"--[==[ Error in 'Interrupt Bar' ]==] return function(...)\"]:1>".to_owned(),
        "[C]: in function 'xpcall'".to_owned(),
        frame(stack_era_c("WeakAuras", "WeakAuras.lua", 4512), "'RunTrigger'"),
    ])
    .counter(57);
    let long = "Modules/Nameplates/Widgets/ThreatIndicator/Indicator.lua";
    let cut = BugEntry::new(
        &format!(
            "{} bad argument #1 to 'SetText' (string expected, got table)",
            truncated(&stack_era_c("SomeUIPack", long, 77))
        ),
        412,
        unix(NOW - 10 * 60),
    )
    .stack(&[frame(stack_era_c("SomeUIPack", long, 77), "'Update'")]);
    BugDb::new(writer, 412)
        .entry(plain)
        .entry(dispatch)
        .entry(aura)
        .entry(cut)
}

/// v10.1 to v12.0.2: string times of the **first** capture. A repeated entry moved to
/// the end keeps its old time, so the array's last entry is older than the one before.
pub fn modern_string_first_seen(writer: Writer) -> BugDb {
    let local = |at| (TimeKind::LocalString, at);
    BugDb::new(writer, 230)
        .entry(
            BugEntry::new(
                &format!(
                    "{} attempt to call a nil value",
                    stack_era_b("Bar", "Bar.lua", 7)
                ),
                228,
                local(NOW - 3 * DAY),
            )
            .stack(&[frame(stack_era_b("Bar", "Bar.lua", 7), "'OnLoad'")]),
        )
        .entry(
            BugEntry::new(
                &format!(
                    "{} attempt to index field '?' (a nil value)",
                    stack_era_b("Foo", "core.lua", 31)
                ),
                230,
                local(NOW - 5 * DAY),
            )
            .stack(&[frame(stack_era_b("Foo", "core.lua", 31), "'Init'")])
            .counter(12),
        )
}

/// v12.0.3 to v12.0.9: string times of the **last** occurrence, so times rise with the
/// array.
pub fn modern_string_last_seen(writer: Writer) -> BugDb {
    let local = |at| (TimeKind::LocalString, at);
    BugDb::new(writer, 315)
        .entry(
            BugEntry::new(
                &format!(
                    "{} attempt to call a nil value",
                    stack_era_b("Bar", "Bar.lua", 7)
                ),
                312,
                local(NOW - 3 * DAY),
            )
            .stack(&[frame(stack_era_b("Bar", "Bar.lua", 7), "'OnLoad'")]),
        )
        .entry(
            BugEntry::new(
                &format!(
                    "{} attempt to index field '?' (a nil value)",
                    stack_era_b("Foo", "core.lua", 31)
                ),
                315,
                local(NOW - 2 * HOUR),
            )
            .stack(&[frame(stack_era_b("Foo", "core.lua", 31), "'Init'")])
            .counter(12),
        )
}

/// r152 (3.3.5a): a message over [`CHUNK_BYTES`] in 2 chunks with its stack and locals
/// embedded, an `event` entry, the legacy keys, and r152's path forms.
pub fn r152_chunked(writer: Writer) -> BugDb {
    let frames = [
        frame(stack_r152_addon("Foo", "1.2", "core.lua", 12), "`Refresh'"),
        frame(stack_r152_library("AceEvent", 3, "Foo", 120), "`?'"),
        "[C]: in function `pcall'".to_owned(),
    ];
    let mut locals = String::from("self = <table> {\n");
    for i in 0..40 {
        locals.push_str(&format!("  field{i:02} = \"value {i:02}\"\n"));
    }
    locals.push('}');
    let error = BugEntry::r152(
        &format!(
            "{} attempt to index a nil value",
            stack_r152_addon("Foo", "1.2", "core.lua", 12)
        ),
        &frames,
        &locals,
        "error",
        17,
        NOW - DAY,
    );
    assert!(error.chunked && error.message.len() <= 2 * CHUNK_BYTES);
    let event = BugEntry::r152(
        "[ADDON_ACTION_BLOCKED] AddOn 'Foo' tried to call the protected function 'TargetUnit()'.",
        &[frame(
            stack_r152_addon("Foo", "1.2", "target.lua", 5),
            "`Pick'",
        )],
        "",
        "event",
        18,
        NOW - HOUR,
    );
    BugDb {
        r152: true,
        ..BugDb::new(writer, 18).legacy_keys()
    }
    .entry(error)
    .entry(event)
}

/// `BugGrabberDB` embedded in `bdUI.lua`, next to the pack's own `BDUI_SAVE` (M3-D44).
pub fn embedded_bdui(writer: Writer) -> Vec<SvFile> {
    let db = BugDb::new(writer, 88).entry(
        BugEntry::new(
            &format!(
                "{} attempt to index a nil value",
                stack_era_c("bdUI", "core/frames.lua", 64)
            ),
            88,
            (TimeKind::Unix, NOW - HOUR),
        )
        .stack(&[frame(stack_era_c("bdUI", "core/frames.lua", 64), "'Skin'")]),
    );
    let save = Table::default()
        .with("profile", Value::str("Default"))
        .with("scale", Value::Num(0.75))
        .with(
            "modules",
            Value::Table(
                Table::default()
                    .push(Value::str("bags"))
                    .push(Value::str("nameplates")),
            ),
        );
    let bytes = lua::render(
        writer,
        &[
            ("BDUI_SAVE", Value::Table(save)),
            ("BugGrabberDB", db.value()),
        ],
    );
    vec![SvFile::account("FAKEACCT", "bdUI.lua", bytes)]
}

/// A `LUA_WARNING` entry (its message starts `LUA_WARNING: `, and it has no `stack` key:
/// its stack is embedded in the message after `|cffffd200Stack:|r`), next to an
/// ordinary error.
pub fn warnings(writer: Writer) -> BugDb {
    let warning = BugEntry::new(
        &format!(
            "LUA_WARNING: {} Couldn't find inherited node \"BackdropTemplate\"\n|cffffd200Stack:|r\n{}",
            stack_era_c("Baz", "Baz.xml", 5),
            frame(stack_era_c("Baz", "Baz.lua", 20), "<main chunk>")
        ),
        64,
        (TimeKind::Unix, NOW - 2 * HOUR),
    );
    let error = BugEntry::new(
        &format!(
            "{} attempt to call a nil value",
            stack_era_c("Baz", "Baz.lua", 33)
        ),
        64,
        (TimeKind::Unix, NOW - HOUR),
    )
    .stack(&[frame(stack_era_c("Baz", "Baz.lua", 33), "'Show'")]);
    BugDb::new(writer, 64).entry(warning).entry(error)
}

/// `ADDON_ACTION_FORBIDDEN`: `!BugGrabber`'s own frames come first, and the guilty
/// addon is the first quoted name in the message.
pub fn forbidden(writer: Writer) -> BugDb {
    BugDb::new(writer, 99).entry(
        BugEntry::new(
            "[ADDON_ACTION_FORBIDDEN] AddOn 'Foo' tried to call the protected function 'CastSpellByName()'.",
            99,
            (TimeKind::Unix, NOW - 30 * 60),
        )
        .stack(&[
            frame(
                stack_era_c("!BugGrabber", "BugGrabber.lua", 485),
                "<Interface/AddOns/!BugGrabber/BugGrabber.lua:485>",
            ),
            "[C]: in function 'CastSpellByName'".to_owned(),
            frame(stack_era_c("Foo", "cast.lua", 88), "'Cast'"),
        ])
        .counter(2),
    )
}

/// Personal data where real files hold it: a player GUID and a player link in the
/// message, a character's name in the locals, and `source` (a received report).
pub fn personal(writer: Writer) -> BugDb {
    BugDb::new(writer, 140)
        .entry(
            BugEntry::new(
                &format!(
                    "{} unit Player-1234-0ABCDEF1 (|Hplayer:FakeChar-FakeRealm|h[FakeChar]|h) has no frame",
                    stack_era_c("Foo", "units.lua", 51)
                ),
                140,
                (TimeKind::Unix, NOW - HOUR),
            )
            .stack(&[frame(stack_era_c("Foo", "units.lua", 51), "'Bind'")])
            .locals("unit = \"player\"\nname = \"FakeChar\"\nrealm = \"FakeRealm\""),
        )
        .entry(
            BugEntry::new(
                &format!(
                    "{} attempt to concatenate a nil value",
                    stack_era_c("Bar", "Bar.lua", 9)
                ),
                140,
                (TimeKind::Unix, NOW - 20 * 60),
            )
            .source("FakeChar"),
        )
}

/// BugSack's received reports: two errors another player sent, stamped with the DB's
/// current session (as `OnBugComm` does) and keeping the sender's time and counter,
/// after one local error of an earlier session.
pub fn received(writer: Writer) -> BugDb {
    let local = BugEntry::new(
        &format!(
            "{} attempt to index a nil value",
            stack_era_c("Foo", "core.lua", 12)
        ),
        56,
        (TimeKind::Unix, NOW - DAY),
    )
    .stack(&[frame(stack_era_c("Foo", "core.lua", 12), "'Refresh'")]);
    let sent = |file: &str, line: u32, counter: u32| {
        BugEntry::new(
            &format!(
                "{} attempt to compare nil with number",
                stack_era_c("Details", file, line)
            ),
            57,
            (TimeKind::Unix, NOW - 3 * DAY),
        )
        .stack(&[frame(stack_era_c("Details", file, line), "'Parse'")])
        .counter(counter)
        .source(SENDER)
    };
    BugDb::new(writer, 57)
        .entry(local)
        .entry(sent("core/parser.lua", 1234, 4))
        .entry(sent("core/meta.lua", 90, 1))
}

/// Names no pattern can recognize: an addon's own `error("…")` text, a chunk name
/// holding user code, beside a runtime message naming only an identifier.
pub fn custom_names(writer: Writer) -> BugDb {
    let unix = |at| (TimeKind::Unix, at);
    BugDb::new(writer, 21)
        .entry(
            BugEntry::new(
                &format!(
                    "{} FakeChar on FakeRealm pulled",
                    stack_era_c("RaidAlerts", "alerts.lua", 30)
                ),
                21,
                unix(NOW - 3 * HOUR),
            )
            .stack(&[
                "[C]: in function 'error'".to_owned(),
                frame(stack_era_c("RaidAlerts", "alerts.lua", 30), "'Announce'"),
            ]),
        )
        .entry(BugEntry::new(
            "[string \"print('FakeChar')\"]:1: attempt to call a nil value",
            21,
            unix(NOW - 2 * HOUR),
        ))
        .entry(
            BugEntry::new(
                &format!(
                    "{} attempt to index a nil value (field 'x')",
                    stack_era_c("Foo", "core.lua", 12)
                ),
                21,
                unix(NOW - HOUR),
            )
            .stack(&[frame(stack_era_c("Foo", "core.lua", 12), "'Refresh'")]),
        )
}

/// Two accounts, each with its own database and session counter (M3-D19).
pub fn two_accounts(writer: Writer) -> Vec<SvFile> {
    let entry = |folder: &str, session: u32, at: i64| {
        BugEntry::new(
            &format!(
                "{} attempt to index a nil value",
                stack_era_c(folder, "core.lua", 12)
            ),
            session,
            (TimeKind::Unix, at),
        )
        .stack(&[frame(stack_era_c(folder, "core.lua", 12), "'Refresh'")])
    };
    let first = BugDb::new(writer, 120)
        .entry(entry("Foo", 119, NOW - DAY))
        .entry(entry("Bar", 120, NOW - HOUR));
    let second = BugDb::new(writer, 7).entry(entry("Baz", 7, NOW - 2 * HOUR));
    vec![
        SvFile::account("FAKEACCT", FILE, first.render()),
        SvFile::account(super::privacy::ACCOUNT2_NAME, FILE, second.render()),
    ]
}

/// An integer time, a recent string time and a string time older than a day, in one
/// database.
pub fn mixed_times(writer: Writer) -> BugDb {
    let entry = |line: u32, session: u32, time| {
        BugEntry::new(
            &format!(
                "{} attempt to call a nil value",
                stack_era_c("Foo", "core.lua", line)
            ),
            session,
            time,
        )
        .stack(&[frame(stack_era_c("Foo", "core.lua", line), "'Run'")])
    };
    BugDb::new(writer, 50)
        .entry(entry(5, 46, (TimeKind::LocalString, NOW - 3 * DAY)))
        .entry(entry(6, 49, (TimeKind::LocalString, NOW - 2 * HOUR)))
        .entry(entry(7, 50, (TimeKind::Unix, NOW - HOUR)))
}

/// `entries` errors (the decoders' cap, or one past it), then a malformed tail: a string
/// where an entry belongs, and an entry with the wrong types (a numeric message, a
/// boolean time, no session).
pub fn flood(writer: Writer, entries: usize) -> BugDb {
    let mut db = BugDb::new(writer, 900);
    for i in 0..entries {
        let line = u32::try_from(i % 5000).unwrap() + 1;
        db.entries.push(BugEntry::new(
            &format!(
                "{} attempt to index a nil value (local 'frame{i}')",
                stack_era_c("Flood", "spam.lua", line)
            ),
            900,
            (TimeKind::Unix, NOW - i64::try_from(entries - i).unwrap()),
        ));
    }
    db.tail.push(Value::str("not an entry"));
    db.tail.push(Value::Table(
        Table::default()
            .with("message", Value::Int(42))
            .with("time", Value::Bool(true))
            .with("counter", Value::Int(1)),
    ));
    db
}
