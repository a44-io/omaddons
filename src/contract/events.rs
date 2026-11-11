//! The enumerable stderr vocabulary and the event shapes the reporter writes.

use schemars::JsonSchema;
#[cfg(feature = "schemas")]
use schemars::{Schema, SchemaGenerator};
use serde::Serialize;

use crate::error::{Error, Warning};

macro_rules! wire_enum {
    ($(#[$meta:meta])* pub enum $name:ident { $($(#[$doc:meta])* $variant:ident = $wire:literal,)+ }) => {
        $(#[$meta])*
        pub enum $name { $($(#[$doc])* #[serde(rename = $wire)] $variant,)+ }
        impl $name {
            pub const ALL: &[$name] = &[$(Self::$variant,)+];
            pub const fn wire(self) -> &'static str {
                match self { $(Self::$variant => $wire,)+ }
            }
        }
    };
}

wire_enum! {
    /// The discriminator of an NDJSON stderr event.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, JsonSchema)]
    pub enum EventName {
        Progress = "progress",
        Log = "log",
        Warning = "warning",
        Error = "error",
    }
}

impl EventName {
    /// This event's schema. Use a fresh serialize-contract generator for each event.
    #[cfg(feature = "schemas")]
    pub fn schema(self, g: &mut SchemaGenerator) -> Schema {
        match self {
            Self::Progress => g.root_schema_for::<ProgressEvent<'static>>(),
            Self::Log => g.root_schema_for::<LogEvent<'static>>(),
            Self::Warning => g.root_schema_for::<WarningEvent<'static>>(),
            Self::Error => g.root_schema_for::<ErrorEvent<'static>>(),
        }
    }
}

wire_enum! {
    /// A phase of a long command.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, JsonSchema)]
    pub enum Phase {
        /// Release metadata fetched from a source.
        Fetch = "fetch",
        /// An archive downloaded.
        Download = "download",
        /// An archive verified against a client's load rules, or a backup's files hashed
        /// (under a fixed label, as for `Copy`).
        Verify = "verify",
        /// A target's folders staged.
        Stage = "stage",
        /// A target's transaction committed.
        Commit = "commit",
        /// Files of a `WTF/` tree copied (backups and restores); its message is a fixed
        /// label, never a path, because paths below `WTF/Account/` name the player.
        Copy = "copy",
    }
}

wire_enum! {
    /// A `log` event's level, and the verbosity that shows it (M4-D17).
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
    pub enum LogLevel {
        Info = "info",
        /// Shown only with `-v`.
        Debug = "debug",
        /// Shown only with `-vv`: the release memo's decisions with their probe and
        /// fetch times, discovery's routes, and waits for the global lock. Its messages
        /// are for reading, not parsing.
        Trace = "trace",
    }
}

impl LogLevel {
    /// The `-v` count from which events of this level are shown: the one place the
    /// verbosity levels are numbered (level 1 is `-v`, level 2 `-vv`).
    pub const fn min_verbosity(self) -> u8 {
        match self {
            Self::Info => 0,
            Self::Debug => 1,
            Self::Trace => 2,
        }
    }
}

/// What a `progress` event's item is about, typed, so a reader can tell which of its own
/// rows the event moves without reading the free-text `message` (1n2p.18): the client
/// the item is on and the ref it is about, each `None` when the item is not one client's
/// or one ref's.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Subject<'a> {
    pub target: Option<&'a str>,
    pub r#ref: Option<&'a str>,
}

impl<'a> Subject<'a> {
    /// About no one client or ref: an announcement, a fixed label, a search's sources.
    pub const NONE: Subject<'static> = Subject {
        target: None,
        r#ref: None,
    };

    /// One client's step (its stage, its commit).
    pub const fn target(id: &'a str) -> Self {
        Self {
            target: Some(id),
            r#ref: None,
        }
    }

    /// One ref, on no one client (its release metadata fetched).
    pub const fn of_ref(r#ref: &'a str) -> Self {
        Self {
            target: None,
            r#ref: Some(r#ref),
        }
    }

    /// One ref on one client (its archive downloaded or verified there).
    pub const fn on(r#ref: &'a str, target: &'a str) -> Self {
        Self {
            target: Some(target),
            r#ref: Some(r#ref),
        }
    }
}

/// A `progress` event: a long command's phase, message and optional counts, and what
/// its item is about (`target`, `ref`).
#[derive(Debug, Serialize, JsonSchema)]
pub struct ProgressEvent<'a> {
    event: EventName,
    phase: Phase,
    /// For reading: its wording may change in any release. `target` and `ref` say what
    /// the item is about.
    message: &'a str,
    /// How many of the phase's items (or files) are done, `0` when it is announced;
    /// omitted for an event without counts.
    #[serde(skip_serializing_if = "Option::is_none")]
    current: Option<u64>,
    /// How many the phase expects in all; omitted for an event without counts.
    #[serde(skip_serializing_if = "Option::is_none")]
    total: Option<u64>,
    /// The id of the client the item is on (`stage`, `commit`, and a `download` or
    /// `verify` there); `null` when it is no one client's (a `fetch`, a `copy`, an
    /// announcement).
    target: Option<&'a str>,
    /// The ref the item is about, as `list` names an installed addon's (`gh:o/foo`): a
    /// `fetch` of its release, a `download` or `verify` of its archive; `null` when it
    /// is about no one ref (a `stage`, a `commit`, a `copy`, `search`'s sources).
    #[serde(rename = "ref")]
    r#ref: Option<&'a str>,
}

impl<'a> ProgressEvent<'a> {
    pub(crate) fn new(
        phase: Phase,
        message: &'a str,
        current: Option<u64>,
        total: Option<u64>,
        subject: Subject<'a>,
    ) -> Self {
        Self {
            event: EventName::Progress,
            phase,
            message,
            current,
            total,
            target: subject.target,
            r#ref: subject.r#ref,
        }
    }
}

/// A `log` event: an informational or verbose diagnostic.
#[derive(Debug, Serialize, JsonSchema)]
pub struct LogEvent<'a> {
    event: EventName,
    level: LogLevel,
    message: &'a str,
}

impl<'a> LogEvent<'a> {
    pub(crate) fn new(level: LogLevel, message: &'a str) -> Self {
        Self {
            event: EventName::Log,
            level,
            message,
        }
    }
}

/// A `warning` event: one per warning, mirroring the envelope's `warnings[]`.
#[derive(Debug, Serialize, JsonSchema)]
pub struct WarningEvent<'a> {
    event: EventName,
    code: &'static str,
    message: &'a str,
    fix: Option<&'a str>,
    fix_argv: Option<&'a [String]>,
}

impl<'a> WarningEvent<'a> {
    pub(crate) fn of(warning: &'a Warning) -> Self {
        Self {
            event: EventName::Warning,
            code: warning.code.wire(),
            message: &warning.message,
            fix: warning.fix.as_ref().map(|f| f.display()),
            fix_argv: warning.fix.as_ref().and_then(|f| f.argv()),
        }
    }
}

/// An `error` event: one per failure, mirroring the envelope's error and exit code.
#[derive(Debug, Serialize, JsonSchema)]
pub struct ErrorEvent<'a> {
    event: EventName,
    code: &'static str,
    message: &'a str,
    fix: &'a str,
    fix_argv: Option<&'a [String]>,
    retryable: bool,
    exit_code: u8,
}

impl<'a> ErrorEvent<'a> {
    /// The event of `error` in a run that exits by its own code's exit.
    #[cfg(test)]
    pub(crate) fn of(error: &'a Error) -> Self {
        Self::exiting(error, error.exit().code())
    }

    /// The event of `error` in a run that exits `exit_code` (a doctor command's, from its
    /// own dictionary).
    pub(crate) fn exiting(error: &'a Error, exit_code: u8) -> Self {
        Self {
            event: EventName::Error,
            code: error.code.wire(),
            message: &error.message,
            fix: error.fix.display(),
            fix_argv: error.fix.argv(),
            retryable: error.retryable,
            exit_code,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use schemars::generate::SchemaSettings;
    use serde_json::Value;

    use super::*;
    use crate::error::{ErrorCode, Suggestion, WarningCode};
    use crate::source_scan::{Literals, production_code};

    #[test]
    fn all_lists_are_complete_and_ordered() {
        let cases = [
            (
                "events",
                EventName::ALL
                    .iter()
                    .map(|x| (x.wire(), serde_json::to_value(x).unwrap()))
                    .collect::<Vec<_>>(),
                vec!["progress", "log", "warning", "error"],
            ),
            (
                "phases",
                Phase::ALL
                    .iter()
                    .map(|x| (x.wire(), serde_json::to_value(x).unwrap()))
                    .collect(),
                vec!["fetch", "download", "verify", "stage", "commit", "copy"],
            ),
            (
                "levels",
                LogLevel::ALL
                    .iter()
                    .map(|x| (x.wire(), serde_json::to_value(x).unwrap()))
                    .collect(),
                vec!["info", "debug", "trace"],
            ),
        ];
        for (case, entries, expected) in cases {
            let wires: Vec<_> = entries.iter().map(|(wire, _)| *wire).collect();
            assert_eq!(wires, expected, "{case}: registry order");
            assert_eq!(
                wires.iter().collect::<BTreeSet<_>>().len(),
                wires.len(),
                "{case}: duplicates in {wires:?}"
            );
            for (wire, serialized) in entries {
                assert_eq!(serialized, Value::from(wire), "{case}: serde for {wire}");
            }
        }
    }

    fn warning() -> Warning {
        Warning::new(WarningCode::ReloadRequired, "retail needs a /reload")
    }

    fn error() -> Error {
        Error::new(
            ErrorCode::TargetAmbiguous,
            "weakauras is compatible with 2 targets",
            Suggestion::command(["omaddons", "install", "weakauras", "-t", "retail"]),
        )
    }

    #[test]
    fn events_serialize_byte_for_byte_as_before() {
        let cases = [
            (
                "warning_with_text_fix",
                serde_json::to_string(&WarningEvent::of(
                    &warning().with_fix(Suggestion::text("/reload in game")),
                ))
                .unwrap(),
                r#"{"event":"warning","code":"reload_required","message":"retail needs a /reload","fix":"/reload in game","fix_argv":null}"#,
            ),
            (
                "warning_without_fix",
                serde_json::to_string(&WarningEvent::of(&warning())).unwrap(),
                r#"{"event":"warning","code":"reload_required","message":"retail needs a /reload","fix":null,"fix_argv":null}"#,
            ),
            (
                "warning_with_command_fix",
                serde_json::to_string(&WarningEvent::of(
                    &warning().with_fix(Suggestion::command(["omaddons", "status"])),
                ))
                .unwrap(),
                r#"{"event":"warning","code":"reload_required","message":"retail needs a /reload","fix":"omaddons status","fix_argv":["omaddons","status"]}"#,
            ),
            (
                "error_with_command_fix",
                serde_json::to_string(&ErrorEvent::of(&error())).unwrap(),
                r#"{"event":"error","code":"target_ambiguous","message":"weakauras is compatible with 2 targets","fix":"omaddons install weakauras -t retail","fix_argv":["omaddons","install","weakauras","-t","retail"],"retryable":false,"exit_code":3}"#,
            ),
            (
                "log_info",
                serde_json::to_string(&LogEvent::new(LogLevel::Info, "checking 3 addons")).unwrap(),
                r#"{"event":"log","level":"info","message":"checking 3 addons"}"#,
            ),
            (
                "progress_with_counts",
                serde_json::to_string(&ProgressEvent::new(
                    Phase::Download,
                    "WeakAuras",
                    Some(1),
                    Some(3),
                    Subject::NONE,
                ))
                .unwrap(),
                r#"{"event":"progress","phase":"download","message":"WeakAuras","current":1,"total":3,"target":null,"ref":null}"#,
            ),
            (
                "progress_without_counts",
                serde_json::to_string(&ProgressEvent::new(
                    Phase::Fetch,
                    "gh:o/foo",
                    None,
                    None,
                    Subject::of_ref("gh:o/foo"),
                ))
                .unwrap(),
                r#"{"event":"progress","phase":"fetch","message":"gh:o/foo","target":null,"ref":"gh:o/foo"}"#,
            ),
            (
                "progress_on_a_client",
                serde_json::to_string(&ProgressEvent::new(
                    Phase::Download,
                    "gh:o/foo@v2 on retail",
                    Some(1),
                    Some(2),
                    Subject::on("gh:o/foo", "retail"),
                ))
                .unwrap(),
                r#"{"event":"progress","phase":"download","message":"gh:o/foo@v2 on retail","current":1,"total":2,"target":"retail","ref":"gh:o/foo"}"#,
            ),
            (
                "progress_of_a_client",
                serde_json::to_string(&ProgressEvent::new(
                    Phase::Commit,
                    "retail",
                    Some(1),
                    Some(1),
                    Subject::target("retail"),
                ))
                .unwrap(),
                r#"{"event":"progress","phase":"commit","message":"retail","current":1,"total":1,"target":"retail","ref":null}"#,
            ),
        ];
        for (case, actual, expected) in cases {
            assert_eq!(actual, expected, "{case}: emitted event bytes");
        }
    }

    #[test]
    fn each_event_schema_lists_exactly_its_fields() {
        let cases = [
            (
                EventName::Progress,
                serde_json::to_value(ProgressEvent::new(
                    Phase::Download,
                    "WeakAuras",
                    Some(1),
                    Some(3),
                    Subject::on("gh:o/wa", "retail"),
                ))
                .unwrap(),
                serde_json::to_value(ProgressEvent::new(
                    Phase::Download,
                    "WeakAuras",
                    None,
                    None,
                    Subject::NONE,
                ))
                .unwrap(),
            ),
            (
                EventName::Log,
                serde_json::to_value(LogEvent::new(LogLevel::Info, "checking")).unwrap(),
                serde_json::to_value(LogEvent::new(LogLevel::Debug, "details")).unwrap(),
            ),
            (
                EventName::Warning,
                serde_json::to_value(WarningEvent::of(
                    &warning().with_fix(Suggestion::command(["omaddons", "status"])),
                ))
                .unwrap(),
                serde_json::to_value(WarningEvent::of(&warning())).unwrap(),
            ),
            (
                EventName::Error,
                serde_json::to_value(ErrorEvent::of(&error())).unwrap(),
                serde_json::to_value(ErrorEvent::of(&Error::new(
                    ErrorCode::Usage,
                    "bad value",
                    Suggestion::text("try again"),
                )))
                .unwrap(),
            ),
        ];
        assert_eq!(
            cases
                .iter()
                .map(|(name, _, _)| *name)
                .collect::<BTreeSet<_>>(),
            EventName::ALL.iter().copied().collect(),
            "every event schema has a case"
        );
        for (name, full, minimal) in cases {
            let mut g = SchemaSettings::draft2020_12()
                .for_serialize()
                .into_generator();
            let schema = serde_json::to_value(name.schema(&mut g)).unwrap();
            let keys = |value: &Value| {
                value
                    .as_object()
                    .unwrap()
                    .keys()
                    .cloned()
                    .collect::<BTreeSet<_>>()
            };
            let required: BTreeSet<_> = schema["required"]
                .as_array()
                .unwrap()
                .iter()
                .map(|key| key.as_str().unwrap().to_owned())
                .collect();
            assert_eq!(
                keys(&schema["properties"]),
                keys(&full),
                "{name:?}: schema properties: {schema}"
            );
            assert_eq!(
                required,
                keys(&minimal),
                "{name:?}: required keys: {schema}"
            );
        }
    }

    #[test]
    fn the_event_name_matches_the_struct() {
        let cases = [
            (
                EventName::Progress,
                serde_json::to_value(ProgressEvent::new(
                    Phase::Fetch,
                    "fetching",
                    None,
                    None,
                    Subject::NONE,
                ))
                .unwrap(),
            ),
            (
                EventName::Log,
                serde_json::to_value(LogEvent::new(LogLevel::Info, "checking")).unwrap(),
            ),
            (
                EventName::Warning,
                serde_json::to_value(WarningEvent::of(&warning())).unwrap(),
            ),
            (
                EventName::Error,
                serde_json::to_value(ErrorEvent::of(&error())).unwrap(),
            ),
        ];
        for (name, event) in cases {
            assert_eq!(
                event["event"],
                name.wire(),
                "{name:?}: discriminator in {event}"
            );
        }
    }

    #[test]
    fn min_verbosity_per_level() {
        let cases = [
            ("info", LogLevel::Info, 0),
            ("debug", LogLevel::Debug, 1),
            ("trace", LogLevel::Trace, 2),
        ];
        for (case, level, want) in cases {
            assert_eq!(level.min_verbosity(), want, "{case}: min_verbosity");
            assert_eq!(level.wire(), case, "{case}: wire name");
        }
        assert_eq!(
            serde_json::to_string(&LogLevel::Trace).unwrap(),
            "\"trace\"",
            "trace serializes as its wire name"
        );
    }

    #[test]
    fn stderr_holds_no_event_name_literal() {
        let code = production_code(include_str!("../output/stderr.rs"), Literals::Keep).unwrap();
        for name in EventName::ALL
            .iter()
            .map(|x| x.wire())
            .chain(Phase::ALL.iter().map(|x| x.wire()))
            .chain(LogLevel::ALL.iter().map(|x| x.wire()))
        {
            let literal = format!("\"{name}\"");
            assert!(
                !code.contains(&literal),
                "stderr.rs holds {literal}; use the event registry"
            );
        }
    }
}
