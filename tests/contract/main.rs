//! e2e suite: the CLI contract (envelopes, exits, usage fixes) through the real binary.
//!
//! The M1 CLI contract conformance suite (vision §5.9, §7 M1 contract core): every
//! command, on success and on failure, prints exactly one JSON envelope on stdout with
//! the documented keys in order, an `exit_code` equal to the process exit and to the
//! error code's registered exit, fixes that are either an `omaddons` argv that
//! round-trips through shell-words or a documented non-CLI text, and NDJSON on stderr
//! with exactly one `error` event per failure. Plus the exit-site scan of `src/`.

use crate::common;

mod canonical_names;
#[cfg(test)]
mod details_coverage;
mod doctor_fix_binding;
mod exit_sites;
mod intent_corpus;
mod manifest_report;
mod matrix;
mod recommendations;
#[cfg(test)]
mod registries;
#[cfg(test)]
mod rules;
#[cfg(test)]
mod snapshot_walk;
mod special;
#[cfg(test)]
mod state_files;
#[cfg(test)]
mod strict;
mod usage_fixes;
use crate::validate;
#[cfg(test)]
mod verbose_fields;
#[cfg(test)]
mod verbosity;
#[cfg(test)]
mod wire_names;

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use common::archives::zip_text;
use common::home::{Home, Run};
use common::http::{Asset, gh_release};
use omaddons::error::ErrorCode;
use serde_json::{Value, json};
use support::Scene;

#[cfg(test)]
mod support {
    use super::*;
    use omaddons::contract::actions::Action;
    use omaddons::contract::events::{EventName, LogLevel, Phase};
    use serde::Deserialize;
    use serde::de::{IgnoredAny, MapAccess, Visitor};

    /// Actions whose instances may differ from their published destructive default:
    /// an `apply` hint is as destructive as its plan's command.
    const VARIES: &[Action] = &[Action::Apply];

    /// The envelope's keys in wire order.
    pub const ORDER: &[&str] = &[
        "ok",
        "schema_version",
        "tool_version",
        "command",
        "exit_code",
        "partial",
        "data",
        "error",
        "warnings",
        "commands",
        "meta",
    ];

    /// Prose fixes that mention an omaddons command on purpose, matched as substrings:
    /// keep this list small and explicit.
    pub const PROSE_COMMANDS: &[&str] = &[
        // A token goes on standard input, never argv.
        "omaddons auth github set",
        // Asked for with a bug report.
        "omaddons --version",
        // sync: the file is the user's to regenerate.
        "regenerate the file with `omaddons export`",
        // local_changes_replaced under --dry-run: there is no transaction yet.
        "`omaddons undo` restores them",
    ];

    /// Whether a prose fix `display` names a runnable omaddons command: `omaddons`
    /// then a subcommand or a flag. One that needs a value only the user has shows it
    /// as a `<placeholder>` and is not runnable as is; [`PROSE_COMMANDS`] mention one on
    /// purpose. Such a fix must carry argv instead.
    pub fn names_a_command(display: &str) -> bool {
        if display.contains('<') || PROSE_COMMANDS.iter().any(|p| display.contains(p)) {
            return false;
        }
        display.match_indices("omaddons ").any(|(at, word)| {
            let rest = &display[at + word.len()..];
            let first = rest
                .split(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
                .next()
                .unwrap_or_default();
            rest.starts_with("--")
                || omaddons::app::COMMANDS
                    .iter()
                    .any(|c| c.name.split(' ').next() == Some(first))
        })
    }

    /// The top-level keys of a JSON object, in document order.
    struct Keys(Vec<String>);

    impl<'de> Deserialize<'de> for Keys {
        fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            struct V;
            impl<'de> Visitor<'de> for V {
                type Value = Keys;
                fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    f.write_str("a JSON object")
                }
                fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Keys, A::Error> {
                    let mut keys = Vec::new();
                    while let Some(key) = map.next_key::<String>()? {
                        map.next_value::<IgnoredAny>()?;
                        keys.push(key);
                    }
                    Ok(Keys(keys))
                }
            }
            d.deserialize_map(V)
        }
    }

    /// A run that passed every contract check, with its parsed envelope.
    pub struct Checked {
        pub run: Run,
        pub envelope: Value,
    }

    /// Asserts the whole contract on one `--json` run, and that it ended with `code`: the
    /// general envelope's, or the doctor's for a command of its dictionary (M4-D34).
    pub fn check(run: Run, code: i32) -> Checked {
        let ctx = run.context();
        assert_eq!(run.code, code, "{ctx}");
        let doctor = serde_json::from_str::<Value>(&run.stdout).is_ok_and(|envelope| {
            common::doctor::is_doctor(envelope["command"].as_str().unwrap_or_default())
        });
        let envelope = if doctor {
            common::doctor::check_envelope(&run.stdout, &run.stderr, code)
        } else {
            general(&run, code, &ctx)
        };
        named(&envelope, &run.argv, &ctx);
        assert!(envelope["meta"]["elapsed_ms"].is_u64(), "{ctx}");
        assert!(envelope["meta"]["offline"].is_boolean(), "{ctx}");
        if let Some(error) = envelope.get("error") {
            assert!(
                error["retryable"].is_boolean() && error["details"].is_object(),
                "{ctx}"
            );
            assert!(
                error["message"].as_str().is_some_and(|m| !m.is_empty()),
                "{ctx}"
            );
            fix(&error["fix"], &error["fix_argv"], &ctx);
            for alternative in error["alternatives"].as_array().unwrap() {
                fix(&alternative["display"], &alternative["argv"], &ctx);
            }
        }
        let command = envelope["command"].as_str().unwrap_or_default();
        observe_exit(command, code, &ctx);
        for warning in envelope["warnings"].as_array().unwrap() {
            assert!(
                warning["code"].is_string() && warning["message"].is_string(),
                "{ctx}"
            );
            if !warning["fix"].is_null() {
                fix(&warning["fix"], &warning["fix_argv"], &ctx);
            }
        }
        for command in envelope["commands"].as_array().unwrap() {
            let argv = strings(&command["argv"]);
            assert_eq!(argv.first().map(String::as_str), Some("omaddons"), "{ctx}");
            assert_eq!(command["command"], shell_words::join(&argv), "{ctx}");
            no_presentation_flags(&argv, &ctx);
            parses(&argv, &ctx);
            assert!(
                command["action"].is_string() && command["destructive"].is_boolean(),
                "{ctx}"
            );
            let name = command["action"].as_str().unwrap_or_default();
            let action = Action::from_wire(name);
            assert!(action.is_some(), "unregistered action {name}\n{ctx}");
            let action = action.unwrap();
            if !VARIES.contains(&action) {
                assert_eq!(
                    command["destructive"],
                    action.destructive(),
                    "{name}: destructive default\n{ctx}"
                );
            }
            // An agent trusts `action` without parsing argv: these name the verb run.
            for verb in [Action::Install, Action::Search] {
                if action == verb {
                    assert_eq!(argv.get(1).map(String::as_str), Some(verb.wire()), "{ctx}");
                }
            }
        }

        // NDJSON on stderr, every event registered.
        for line in run.stderr.lines() {
            let event = serde_json::from_str::<Value>(line);
            assert!(event.is_ok(), "stderr line is not JSON: {line}\n{ctx}");
            let event = event.unwrap();
            let kind = event["event"].as_str().unwrap_or_default();
            assert!(
                EventName::ALL.iter().any(|name| name.wire() == kind),
                "event {kind}\n{ctx}"
            );
            if kind == EventName::Progress.wire() {
                assert!(
                    Phase::ALL
                        .iter()
                        .any(|phase| event["phase"] == phase.wire()),
                    "unregistered progress phase {}\n{ctx}",
                    event["phase"]
                );
            }
            if kind == EventName::Log.wire() {
                assert!(
                    LogLevel::ALL
                        .iter()
                        .any(|level| event["level"] == level.wire()),
                    "unregistered log level {}\n{ctx}",
                    event["level"]
                );
            }
        }

        // Every part against its published schema, strictly (M4-D9, M4-D10): a doctor
        // report against its command's data schema.
        {
            let parts = if doctor {
                common::doctor::as_general(&envelope)
            } else {
                envelope.clone()
            };
            let validated = super::validate::validate_envelope(&parts, &run.stderr);
            assert!(
                validated.is_ok(),
                "{}\n{ctx}",
                validated.err().unwrap_or_default()
            );
        }
        Checked { run, envelope }
    }

    /// The general envelope's own rules: one document, its keys in [`ORDER`], an `error`
    /// exactly on a failure, at its code's registered exit, `data` on success and on
    /// `check_failed`, and one stderr `error` event per failure.
    fn general(run: &Run, code: i32, ctx: &str) -> Value {
        // Exactly one JSON document on stdout.
        let docs: Result<Vec<Value>, _> = serde_json::Deserializer::from_str(&run.stdout)
            .into_iter::<Value>()
            .collect();
        assert!(docs.is_ok(), "stdout is not JSON\n{ctx}");
        let docs = docs.unwrap();
        assert_eq!(docs.len(), 1, "exactly one envelope\n{ctx}");
        let envelope = docs.into_iter().next().unwrap();

        // Keys in wire order.
        let keys: Keys = serde_json::from_str(&run.stdout).unwrap();
        let expected: Vec<&str> = ORDER
            .iter()
            .copied()
            .filter(|k| keys.0.iter().any(|have| have == k))
            .collect();
        assert_eq!(keys.0, expected, "envelope key order\n{ctx}");

        // ok, exit_code and the registered exit agree.
        assert_eq!(
            envelope["schema_version"],
            omaddons::output::SCHEMA_VERSION,
            "{ctx}"
        );
        assert_eq!(envelope["exit_code"], code, "{ctx}");
        assert_eq!(envelope["ok"], code == 0, "{ctx}");
        if code == 0 {
            assert!(
                envelope.get("error").is_none() && envelope.get("data").is_some(),
                "{ctx}"
            );
        } else {
            let error = &envelope["error"];
            let wire = error["code"].as_str().unwrap_or_default();
            let registered = serde_json::from_value::<ErrorCode>(Value::from(wire));
            assert!(
                registered.is_ok(),
                "{wire:?} is not a registered code\n{ctx}"
            );
            let registered = registered.unwrap();
            assert_eq!(
                i32::from(registered.exit().code()),
                code,
                "registered exit of {wire}\n{ctx}"
            );
            if code == 6 {
                assert!(
                    envelope.get("data").is_some(),
                    "check_failed keeps data\n{ctx}"
                );
            }
        }

        // Exactly one error event per failure.
        let errors = run
            .stderr
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter(|event| event["event"] == EventName::Error.wire())
            .count();
        assert_eq!(
            errors,
            usize::from(code != 0),
            "error events on stderr\n{ctx}"
        );
        envelope
    }

    /// A fix is non-empty; its argv starts with omaddons and joins back to the display,
    /// or it is null for prose that names no runnable command ([`names_a_command`]).
    pub fn fix(display: &Value, argv: &Value, ctx: &str) {
        let display = display.as_str().unwrap_or_default();
        assert!(!display.is_empty(), "empty fix\n{ctx}");
        if argv.is_null() {
            assert!(
                !names_a_command(display),
                "a command fix without argv: {display}\n{ctx}"
            );
            return;
        }
        let argv = strings(argv);
        assert_eq!(
            argv.first().map(String::as_str),
            Some("omaddons"),
            "{display}\n{ctx}"
        );
        assert_eq!(
            shell_words::join(&argv),
            display,
            "fix argv round trip\n{ctx}"
        );
        no_presentation_flags(&argv, ctx);
        parses(&argv, ctx);
    }

    /// The envelope `command` an argv that parses must carry (M4-D55 rule 5): `version`
    /// for -V, `help` for -h, else the canonical path of the parsed command. Both flags
    /// are omaddons' own (they parse; dispatch answers them before the command, -V
    /// first), so they are read from the parse, not from clap's display errors.
    pub fn canonical_name(argv: &[String]) -> Option<&'static str> {
        use clap::Parser as _;
        use clap::error::ErrorKind;
        let program = std::iter::once("omaddons".to_owned());
        match omaddons::cli::Cli::try_parse_from(program.chain(argv.iter().cloned())) {
            Ok(cli) if cli.global.version => Some("version"),
            Ok(cli) if cli.global.help => Some("help"),
            Ok(cli) => Some(omaddons::app::command_name(cli.command.as_ref())),
            Err(e) => match e.kind() {
                ErrorKind::DisplayVersion => Some("version"),
                ErrorKind::DisplayHelp => Some("help"),
                _ => None,
            },
        }
    }

    /// Every (command, exit) the contract suite produced in this process.
    static OBSERVED: std::sync::Mutex<BTreeMap<String, std::collections::BTreeSet<i32>>> =
        std::sync::Mutex::new(BTreeMap::new());

    /// Records `code` for `command`, and fails unless the command declares it (its
    /// `COMMANDS` row, or an `ENVELOPE_ONLY` name). Checked inside `check`, so every
    /// envelope of every contract test counts, whatever order the tests run in.
    pub fn observe_exit(command: &str, code: i32, ctx: &str) {
        let declared = omaddons::app::declared_exits(command);
        let so_far = {
            let mut observed = OBSERVED
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            observed.entry(command.to_owned()).or_default().insert(code);
            observed.get(command).cloned().unwrap_or_default()
        };
        assert!(
            declared.is_some(),
            "envelope command {command:?} is neither a COMMANDS path nor an ENVELOPE_ONLY name\n{ctx}"
        );
        let declared = declared.unwrap_or_default();
        let listed = u8::try_from(code).is_ok_and(|c| declared.contains(&c));
        assert!(
            listed,
            "{command} exited {code}, which its COMMANDS row does not declare: declared {declared:?}, observed so far {so_far:?}; add the code to the row's exits if the command can return it\n{ctx}"
        );
    }

    /// A copy of the table, for logs.
    pub fn observed() -> BTreeMap<String, std::collections::BTreeSet<i32>> {
        OBSERVED
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// The envelope of an argv that parses names its canonical path. A panic is reported
    /// under the name known when it struck, so `internal` is left out.
    fn named(envelope: &Value, argv: &[String], ctx: &str) {
        if envelope["error"]["code"] == "internal" {
            return;
        }
        if let Some(expected) = canonical_name(argv) {
            let got = &envelope["command"];
            assert_eq!(
                got, expected,
                "envelope command {got:?} is not the canonical path {expected:?} of {argv:?}\n{ctx}"
            );
        }
    }

    /// A suggested argv parses as an omaddons command line (a help or version request
    /// counts). Parsing only: a command that would then fail (an addon that is not
    /// installed) is fine.
    pub fn parses(argv: &[String], ctx: &str) {
        use clap::Parser as _;
        use clap::error::ErrorKind;
        let failure = omaddons::cli::Cli::try_parse_from(argv)
            .err()
            .filter(|e| !matches!(e.kind(), ErrorKind::DisplayHelp | ErrorKind::DisplayVersion))
            .map(|e| e.to_string());
        assert!(
            failure.is_none(),
            "a suggestion does not parse: {argv:?}\n{}\n{ctx}",
            failure.unwrap_or_default()
        );
    }

    /// A suggested argv runs as is: none of the failing run's presentation flags.
    fn no_presentation_flags(argv: &[String], ctx: &str) {
        let flags = [
            "--json",
            "--robot",
            "--format",
            "--progress",
            "--no-color",
            "--no-progress",
        ];
        let carried: Vec<&String> = argv
            .iter()
            .filter(|a| {
                flags
                    .iter()
                    .any(|f| *a == f || a.starts_with(&format!("{f}=")))
            })
            .collect();
        assert!(
            carried.is_empty(),
            "a suggestion carries {carried:?}: {argv:?}\n{ctx}"
        );
    }

    pub fn strings(value: &Value) -> Vec<String> {
        value
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|s| s.as_str().unwrap_or_default().to_owned())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The envelope with what changes between runs replaced, for snapshots.
    pub fn stable(envelope: &Value, root: &Path) -> Value {
        let root = root.display().to_string();
        let mut value = envelope.clone();
        if let Some(map) = value.as_object_mut() {
            map.remove("meta");
            map.insert("tool_version".into(), Value::from("<version>"));
        }
        scrub(&mut value, &root);
        value
    }

    fn scrub(value: &mut Value, root: &str) {
        match value {
            Value::String(s) => *s = redact(s, root),
            Value::Array(items) => items.iter_mut().for_each(|v| scrub(v, root)),
            Value::Object(map) => {
                for (key, v) in map.iter_mut() {
                    if matches!(key.as_str(), "duration_ms" | "elapsed_ms" | "lock_held_ms") {
                        *v = Value::from(0);
                    } else if key == "build_id" && v.is_string() {
                        // Every build has its own.
                        *v = Value::from("<build_id>");
                    } else {
                        scrub(v, root);
                    }
                }
            }
            _ => {}
        }
    }

    fn crockford(c: char) -> bool {
        c.is_ascii_digit() || (c.is_ascii_uppercase() && !"ILOU".contains(c))
    }

    /// A doctor run id's time, `d` for a digit, before its `__` and ULID.
    const RUN_TIME: &str = "dddd-dd-ddTdd-dd-ddZ__";

    /// Replaces every doctor run id (`2026-10-30T18-05-00Z__<ULID>`), alone or inside a
    /// path, with `<run>`: its ULID and its time both vary.
    fn redact_runs(s: &str) -> String {
        let chars: Vec<char> = s.chars().collect();
        let time = RUN_TIME.chars().count();
        let len = time + 26;
        let is_run = |at: usize| {
            at + len <= chars.len()
                && (at == 0 || !chars[at - 1].is_ascii_alphanumeric())
                && chars
                    .get(at + len)
                    .is_none_or(|c| !c.is_ascii_alphanumeric())
                && RUN_TIME
                    .chars()
                    .zip(&chars[at..])
                    .all(|(want, have)| match want {
                        'd' => have.is_ascii_digit(),
                        _ => want == *have,
                    })
                && chars[at + time..at + len].iter().all(|c| crockford(*c))
        };
        let mut out = String::new();
        let mut i = 0;
        while i < chars.len() {
            if is_run(i) {
                out.push_str("<run>");
                i += len;
            } else {
                out.push(chars[i]);
                i += 1;
            }
        }
        out
    }

    /// Replaces the tempdir root, doctor run ids, ULIDs, RFC 3339 times and precondition
    /// tokens (they hash file stamps) in a string.
    fn redact(s: &str, root: &str) -> String {
        let s = s.replace(root, "<root>");
        if s.parse::<jiff::Timestamp>().is_ok() {
            return "<time>".to_owned();
        }
        if omaddons::app::precondition::is_token(&s) {
            return "<precondition>".to_owned();
        }
        let s = redact_runs(&s);
        let chars: Vec<char> = s.chars().collect();
        let mut out = String::new();
        let mut i = 0;
        while i < chars.len() {
            let run = chars[i..].iter().take_while(|c| crockford(**c)).count();
            let boundary = i == 0 || !chars[i - 1].is_ascii_alphanumeric();
            if run == 26 && boundary {
                out.push_str("<ulid>");
                i += 26;
            } else {
                out.push(chars[i]);
                i += 1;
            }
        }
        out
    }

    /// A retail and a Mists Classic client, fixture zips, and replayed GitHub releases:
    /// WeakAuras (catalog; mainline and mists), gh:o/foo (mainline), gh:o/bar (vanilla).
    pub struct Scene {
        pub home: Home,
    }

    impl Scene {
        pub fn new() -> Self {
            let home = Home::new(&["wow", "wow_classic"]);
            let dl = home.env.root().join("dl");
            fs::create_dir_all(&dl).unwrap();
            let both = "## Interface: 120105, 50504\n## Version: 3\n";
            let retail = "## Interface: 120105\n## Version: 1\n";
            for (name, entries) in [
                ("foo", vec![("Foo/Foo.toc", retail)]),
                (
                    "foo2",
                    vec![("Foo/Foo.toc", "## Interface: 120105\n## Version: 2\n")],
                ),
                ("both", vec![("Both/Both.toc", both)]),
                ("blizz", vec![("Blizzard_Foo/Blizzard_Foo.toc", retail)]),
            ] {
                fs::write(dl.join(format!("{name}.zip")), zip_text(&entries)).unwrap();
            }
            let wa: [Asset<'_>; 1] = [(
                "WeakAuras.zip",
                &[("mainline", 120_105), ("mists", 50_504)],
                vec![("WeakAuras/WeakAuras.toc", both)],
            )];
            gh_release(
                &home.http,
                "https://github.com/WeakAuras/WeakAuras2",
                "5.19.2",
                &wa,
            );
            // What `info`'s `about` says of it.
            common::http::gh_repo(
                &home.http,
                &common::http::RepoMeta {
                    owner: "WeakAuras",
                    name: "WeakAuras2",
                    description: Some("World of Warcraft addon that displays auras"),
                    stars: 1_234,
                    pushed_at: "2026-10-28T09:30:00Z",
                },
            );
            let foo: [Asset<'_>; 1] = [(
                "Foo.zip",
                &[("mainline", 120_105)],
                vec![("Foo/Foo.toc", retail)],
            )];
            gh_release(&home.http, "https://github.com/o/foo", "v1", &foo);
            let vanilla = "## Interface: 11509\n";
            let bar: [Asset<'_>; 1] = [(
                "Bar.zip",
                &[("vanilla", 11_509)],
                vec![("Bar/Bar_Vanilla.toc", vanilla)],
            )];
            gh_release(&home.http, "https://github.com/o/bar", "v1", &bar);
            Self { home }
        }

        pub fn zip(&self, name: &str) -> String {
            self.home
                .env
                .root()
                .join("dl")
                .join(format!("{name}.zip"))
                .display()
                .to_string()
        }

        /// `omaddons <args> --json`, checked against the contract.
        pub fn json(&self, args: &[&str], code: i32) -> Checked {
            check(self.home.run(args), code)
        }

        /// The same with extra environment variables.
        pub fn json_env(&self, args: &[&str], env: &[(&str, &str)], code: i32) -> Checked {
            let mut argv: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
            argv.push("--json".to_owned());
            check(self.home.exec_with(&argv, env), code)
        }

        /// LittleWigs requiring BigWigs and Nowhere (which nothing ships, Tukui and
        /// WoWInterface included), Plug with a bridge folder requiring Masque, and a
        /// catalog naming them (and two projects called Foo).
        pub fn dependencies(&self) {
            use super::common::graph::{Folder, Node, release_graph};
            let folder = |name, deps| Folder {
                name,
                interface: "120105",
                deps,
                optional: &[],
            };
            let node = |base, folders| Node {
                base,
                tag: "v1",
                folders,
            };
            release_graph(
                &self.home.http,
                &[
                    node(
                        "https://github.com/o/littlewigs",
                        &[folder("LittleWigs", &["BigWigs", "Nowhere"])],
                    ),
                    node("https://github.com/o/bigwigs", &[folder("BigWigs", &[])]),
                    node(
                        "https://github.com/o/plug",
                        &[folder("Plug", &[]), folder("Plug_Masque", &["Masque"])],
                    ),
                ],
            );
            let catalog = self.home.env.config().join("omaddons/catalog.d");
            fs::create_dir_all(&catalog).unwrap();
            let mut text = String::from("schema_version = \"1.0\"\n");
            // Foo is another project's catalog name (one file naming it twice would be
            // refused: two entries of one name are variants).
            for (name, repo, folder) in [
                ("LittleWigs", "littlewigs", "LittleWigs"),
                ("BigWigs", "bigwigs", "BigWigs"),
                ("Masque", "masque", "Masque"),
                ("Foo", "foo-two", "FooTwo"),
            ] {
                text.push_str(&format!(
                    "\n[[addon]]\nname = \"{name}\"\nref = \"gh:o/{repo}\"\nfolders = [\"{folder}\"]\n"
                ));
            }
            fs::write(catalog.join("50-audit.toml"), text).unwrap();
            super::common::tukui::tukui_list(&self.home.http, &[super::common::tukui::elvui()]);
            super::common::wowi::wowi_filelist(&self.home.http, &[]);
        }

        /// The same with `input` on standard input.
        pub fn json_input(&self, args: &[&str], input: &[u8], code: i32) -> Checked {
            let mut argv: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
            argv.push("--json".to_owned());
            check(self.home.exec_input(&argv, &[], input), code)
        }

        /// Installs the `foo` zip into retail; returns the tx.
        pub fn install_foo(&self) -> String {
            let zip = self.zip("foo");
            let c = self.json(&["install", "gh:o/foo", "--file", &zip, "-t", "retail"], 0);
            c.envelope["data"]["tx"].as_str().unwrap().to_owned()
        }

        /// The snapshot form of an envelope.
        pub fn stable(&self, envelope: &Value) -> Value {
            stable(envelope, self.home.env.root())
        }
    }

    /// This build's semantic manifest, built once per process; an id two collectors
    /// produced differently fails the test that asks for it.
    pub fn manifest() -> &'static omaddons::contract::manifest::Manifest {
        use omaddons::contract::Registry;
        use omaddons::contract::manifest::{Manifest, build_checked};
        static MANIFEST: std::sync::OnceLock<Manifest> = std::sync::OnceLock::new();
        MANIFEST.get_or_init(|| {
            let (manifest, conflicts) = build_checked(&Registry::builtin());
            assert!(
                conflicts.is_empty(),
                "ids two collectors produced differently: {conflicts:#?}"
            );
            manifest
        })
    }

    /// An envelope without `meta`, for byte comparisons.
    pub fn without_meta(stdout: &str) -> String {
        let mut value: Value = serde_json::from_str(stdout).unwrap();
        if let Some(map) = value.as_object_mut() {
            map.remove("meta");
        }
        value.to_string()
    }

    /// Every `.rs` file under `src/` with its text.
    pub fn sources() -> Vec<(std::path::PathBuf, String)> {
        let mut out = Vec::new();
        let mut stack = vec![Path::new(env!("CARGO_MANIFEST_DIR")).join("src")];
        while let Some(dir) = stack.pop() {
            for entry in fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    let text = fs::read_to_string(&path).unwrap();
                    out.push((path, text));
                }
            }
        }
        out
    }

    /// Counts per error code, for the harness self-test.
    pub fn codes(runs: &[&Checked]) -> BTreeMap<String, usize> {
        let mut out = BTreeMap::new();
        for c in runs {
            if let Some(code) = c.envelope["error"]["code"].as_str() {
                *out.entry(code.to_owned()).or_default() += 1;
            }
        }
        out
    }
}

#[test]
fn the_harness_rejects_broken_envelopes() {
    let run = |stdout: &str, code| Run {
        argv: vec!["test".into()],
        code,
        stdout: stdout.into(),
        stderr: String::new(),
    };
    // Each case fails for its own reason, not an earlier check's.
    let rejects = |failing: Run, code: i32, phrase: &str| {
        let caught = std::panic::catch_unwind(|| support::check(failing, code));
        let message = match caught {
            Ok(_) => String::new(),
            Err(panic) => panic
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| panic.downcast_ref::<&str>().map(|s| (*s).to_owned()))
                .unwrap_or_default(),
        };
        assert!(
            message.contains(phrase),
            "expected a failure for {phrase:?}, got {message:?}"
        );
    };
    let argv = |words: &[&str]| -> Vec<String> { words.iter().map(|w| (*w).to_owned()).collect() };
    // A bare help's data, so the good envelope below also passes schema validation.
    let two = &current(
        r#"{"ok":true,"schema_version":"1.0","tool_version":"x","command":"help","exit_code":0,"data":{"name":"omaddons","version":"x","usage":"omaddons <COMMAND>","commands":[],"global_flags":[],"exit_codes":[],"discovery":[]},"warnings":[],"commands":[],"meta":{"elapsed_ms":0,"offline":false}}"#,
    );
    let doubled = format!("{two}\n{two}");
    rejects(run(&doubled, 0), 0, "exactly one envelope");
    // `info` declares 3, so these fail for their fix, not their exit.
    let empty_fix = &current(
        r#"{"ok":false,"schema_version":"1.0","tool_version":"x","command":"info","exit_code":3,"error":{"code":"not_found","message":"m","fix":"","fix_argv":null,"alternatives":[],"retryable":false,"details":{}},"warnings":[],"commands":[],"meta":{"elapsed_ms":0,"offline":false}}"#,
    );
    let mut failing = run(empty_fix, 3);
    failing.argv = argv(&["info", "x", "--json"]);
    failing.stderr = r#"{"event":"error"}"#.into();
    rejects(failing, 3, "empty fix");
    let unparsable = &current(
        r#"{"ok":false,"schema_version":"1.0","tool_version":"x","command":"info","exit_code":3,"error":{"code":"not_found","message":"m","fix":"omaddons targets pin a b","fix_argv":["omaddons","targets","pin","a","b"],"alternatives":[],"retryable":false,"details":{}},"warnings":[],"commands":[],"meta":{"elapsed_ms":0,"offline":false}}"#,
    );
    let mut failing = run(unparsable, 3);
    failing.argv = argv(&["info", "x", "--json"]);
    failing.stderr = r#"{"event":"error"}"#.into();
    rejects(failing, 3, "does not parse");
    let reordered = &current(
        r#"{"schema_version":"1.0","ok":true,"tool_version":"x","command":"help","exit_code":0,"data":{},"warnings":[],"commands":[],"meta":{"elapsed_ms":0,"offline":false}}"#,
    );
    rejects(run(reordered, 0), 0, "envelope key order");
    // An envelope that names another command than the argv it ran.
    let misnamed = &current(
        r#"{"ok":true,"schema_version":"1.0","tool_version":"x","command":"status","exit_code":0,"data":{},"warnings":[],"commands":[],"meta":{"elapsed_ms":0,"offline":false}}"#,
    );
    let mut listed = run(misnamed, 0);
    listed.argv = argv(&["list", "--json"]);
    rejects(listed, 0, "canonical path");
    // A name that is no command, and an exit its row does not declare.
    let unknown = &current(
        r#"{"ok":true,"schema_version":"1.0","tool_version":"x","command":"auth","exit_code":0,"data":{},"warnings":[],"commands":[],"meta":{"elapsed_ms":0,"offline":false}}"#,
    );
    rejects(run(unknown, 0), 0, "neither a COMMANDS path");
    let undeclared = &current(
        r#"{"ok":false,"schema_version":"1.0","tool_version":"x","command":"help","exit_code":4,"error":{"code":"game_running","message":"m","fix":"quit the game client, then rerun","fix_argv":null,"alternatives":[],"retryable":false,"details":{}},"warnings":[],"commands":[],"meta":{"elapsed_ms":0,"offline":false}}"#,
    );
    let mut failing = run(undeclared, 4);
    failing.stderr = r#"{"event":"error"}"#.into();
    rejects(failing, 4, "does not declare");
    let mut good = run(two, 0);
    good.argv = argv(&["--json"]);
    let ok = support::check(good, 0);
    assert!(support::codes(&[&ok]).is_empty());
    let recorded = support::observed();
    assert!(
        recorded.get("help").is_some_and(|exits| exits.contains(&0)),
        "the good envelope was recorded: {recorded:?}"
    );
    doctor_envelopes();
}

/// `envelope`, a literal written at contract 1.0, with this build's `schema_version`, so
/// a hand-built envelope fails only for the fault it plants.
fn current(envelope: &str) -> String {
    envelope.replace(
        r#""schema_version":"1.0""#,
        &format!(r#""schema_version":"{}""#, omaddons::output::SCHEMA_VERSION),
    )
}

/// The doctor mode (M4-D34, M4-D40), on literal envelopes: no `COMMANDS` row is the
/// doctor's until its first command lands, so these call its check directly.
fn doctor_envelopes() {
    use common::doctor::check_envelope;
    let message = |stdout: &str, stderr: &str, code: i32| {
        let caught = std::panic::catch_unwind(|| check_envelope(stdout, stderr, code));
        match caught {
            Ok(_) => "no panic".to_owned(),
            Err(panic) => panic
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| panic.downcast_ref::<&str>().map(|s| (*s).to_owned()))
                .unwrap_or_default(),
        }
    };
    let rejects = |name: &str, stdout: &str, stderr: &str, code: i32, phrase: &str| {
        let got = message(stdout, stderr, code);
        assert!(
            got.contains(phrase),
            "{name}: expected a failure for {phrase:?}, got {got:?}"
        );
    };
    let head = |ok: bool, exit: i32| {
        let version = omaddons::output::SCHEMA_VERSION;
        format!(
            r#""ok":{ok},"schema_version":"{version}","tool_version":"x","command":"doctor diagnose","exit_code":{exit}"#
        )
    };
    let tail = r#""warnings":[],"commands":[],"meta":{"elapsed_ms":0,"offline":false}"#;
    let hint = r#"{"action":"inspect","command":"omaddons doctor explain fm-x","argv":["omaddons","doctor","explain","fm-x"],"destructive":false}"#;
    let report = r#""state":"DONE_FINDINGS","next_steps":["omaddons doctor explain fm-x"]"#;
    let error = |code: &str| {
        format!(
            r#""error":{{"code":"{code}","message":"m","fix":"omaddons help doctor","fix_argv":["omaddons","help","doctor"],"alternatives":[],"retryable":false,"details":{{}}}}"#
        )
    };
    let event = r#"{"event":"error","code":"usage"}"#;

    let healthy = format!(
        r#"{{{},"state":"DONE_OK","next_steps":[],{tail}}}"#,
        head(true, 0)
    );
    let findings = format!(
        r#"{{{},{report},"warnings":[],"commands":[{hint}],"meta":{{"elapsed_ms":0,"offline":false}}}}"#,
        head(false, 1)
    );
    let usage = format!(r#"{{{},{},{tail}}}"#, head(false, 64), error("usage"));
    let cannot = format!(r#"{{{},{},{tail}}}"#, head(false, 73), error("io"));
    for (name, stdout, stderr, code) in [
        ("healthy", &healthy, "", 0),
        ("findings", &findings, "", 1),
        ("usage", &usage, event, 64),
        ("run directory", &cannot, event, 73),
    ] {
        assert_eq!(message(stdout, stderr, code), "no panic", "{name}");
    }

    let with_data = healthy.replacen(r#""state""#, r#""data":{},"state""#, 1);
    rejects("data", &with_data, "", 0, "has no `data`");
    let findings_error = format!(
        r#"{{{},{report},{},"warnings":[],"commands":[{hint}],"meta":{{"elapsed_ms":0,"offline":false}}}}"#,
        head(false, 1),
        error("check_failed")
    );
    rejects(
        "error on findings",
        &findings_error,
        event,
        1,
        "an `error` exactly on exits",
    );
    let bare = format!(r#"{{{},{tail}}}"#, head(false, 64));
    rejects(
        "usage without error",
        &bare,
        "",
        64,
        "an `error` exactly on exits",
    );
    let late = format!(
        r#"{{{},"state":"DONE_OK","warnings":[],"late":1,"commands":[],"meta":{{"elapsed_ms":0,"offline":false}}}}"#,
        head(true, 0)
    );
    rejects(
        "report key after warnings",
        &late,
        "",
        0,
        "a report key after the envelope's trailing keys",
    );
    let diverging = findings.replacen("fm-x\"]", "fm-y\"]", 1);
    rejects(
        "next_steps",
        &diverging,
        "",
        1,
        "`next_steps` mirrors `commands[].command`",
    );
    let misfiled = format!(r#"{{{},{},{tail}}}"#, head(false, 74), error("usage"));
    rejects(
        "usage at 74",
        &misfiled,
        event,
        74,
        "the doctor exit of usage is 64",
    );
    let outside = format!(r#"{{{},{},{tail}}}"#, head(false, 65), error("usage"));
    rejects(
        "an exit outside the dictionary",
        &outside,
        event,
        65,
        "not in the doctor's dictionary",
    );
    rejects("no error event", &usage, "", 64, "stderr error events");
}

/// Doctor run ids vary with their ULID and time, alone or inside a path.
#[test]
fn stable_redacts_doctor_runs() {
    let run = "2026-10-30T18-05-00Z__01M8Q2VA7K3N5P8R1T4W6X9Y0Z";
    let envelope = json!({
        "run": run,
        "path": format!("/fixture-root/state/omaddons/doctor/runs/{run}/report.json"),
        "tx": "01M8Q2VA7K3N5P8R1T4W6X9Y0Z",
        "near": format!("x{run}"),
    });
    let stable = support::stable(&envelope, Path::new("/fixture-root"));
    assert_eq!(stable["run"], "<run>");
    assert_eq!(
        stable["path"],
        "<root>/state/omaddons/doctor/runs/<run>/report.json"
    );
    assert_eq!(stable["tx"], "<ulid>", "a bare ULID is still a ULID");
    assert_eq!(
        stable["near"], "x2026-10-30T18-05-00Z__<ulid>",
        "no run id without a boundary before it"
    );
}

/// `envelope` as the binary prints it: its top-level keys in wire order (a
/// `serde_json::Value` would sort them).
fn wire_text(envelope: &Value) -> String {
    let fields: Vec<String> = support::ORDER
        .iter()
        .filter_map(|key| {
            envelope
                .get(*key)
                .map(|v| format!("{}:{v}", Value::from(*key)))
        })
        .collect();
    format!("{{{}}}", fields.join(","))
}

/// What `check` panicked with, or "no panic".
fn check_message(run: Run, code: i32) -> String {
    match std::panic::catch_unwind(|| support::check(run, code)) {
        Ok(_) => "no panic".to_owned(),
        Err(panic) => panic
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| panic.downcast_ref::<&str>().map(|s| (*s).to_owned()))
            .unwrap_or_default(),
    }
}

#[test]
fn the_harness_rejects_schema_violations() {
    use common::tlog::tlog;
    let log = tlog!();
    let s = Scene::new();

    log.step("bases");
    let status = log.run(&s.home, &["status", "--json"]);
    let zip = s.zip("foo");
    let install = log.run(
        &s.home,
        &[
            "install", "gh:o/foo", "--file", &zip, "-t", "retail", "--json",
        ],
    );
    let pin = log.run(&s.home, &["pin", "Foo", "-t", "retail", "--json"]);
    let missing = log.run(&s.home, &["status", "-t", "nosuch", "--json"]);
    let mut bases = vec![(status, 0), (install, 0), (pin, 0), (missing, 3)];
    for (ran, code) in &bases {
        support::check(ran.to_run(), *code);
    }
    let envelope =
        |index: usize| -> Value { serde_json::from_str(&bases[index].0.run.stdout).unwrap() };
    let rerun = |index: usize, envelope: &Value, stderr: Option<&str>| Run {
        argv: bases[index].0.run.argv.clone(),
        code: bases[index].1,
        stdout: wire_text(envelope),
        stderr: stderr.map_or_else(|| bases[index].0.run.stderr.clone(), str::to_owned),
    };
    // Failures built by hand; each base passes A9's own check of its details and `check`.
    let failure = |command: &str, argv: &[&str], code: ErrorCode, details: Value| -> Run {
        let exit = i32::from(code.exit().code());
        let envelope = json!({
            "ok": false, "schema_version": omaddons::output::SCHEMA_VERSION,
            "tool_version": env!("CARGO_PKG_VERSION"),
            "command": command, "exit_code": exit,
            "error": {
                "code": code.wire(), "message": "m", "fix": "preview again", "fix_argv": null,
                "alternatives": [], "retryable": false, "details": details,
            },
            "warnings": [], "commands": [], "meta": {"elapsed_ms": 0, "offline": false},
        });
        let event = json!({
            "event": "error", "code": code.wire(), "message": "m", "fix": "preview again",
            "fix_argv": null, "retryable": false, "exit_code": exit,
        });
        Run {
            argv: argv.iter().map(|a| (*a).to_owned()).collect(),
            code: exit,
            stdout: wire_text(&envelope),
            stderr: event.to_string(),
        }
    };
    let stale = |changed: Value| {
        failure(
            "install",
            &["install", "x", "--json"],
            ErrorCode::PlanStale,
            json!({"changed": changed}),
        )
    };
    let unsupported = |reason: &str| {
        failure(
            "backup restore",
            &["backup", "restore", "01M6Q2VA", "-t", "retail", "--json"],
            ErrorCode::FilesystemUnsupported,
            json!({"reason": reason, "backup_dir": "<root>/b"}),
        )
    };
    for base in [
        stale(json!(["selection"])),
        unsupported("no_rename_exchange"),
    ] {
        let error = serde_json::from_str::<Value>(&base.stdout).unwrap()["error"].clone();
        let code: ErrorCode = serde_json::from_value(error["code"].clone()).unwrap();
        assert_eq!(
            code.check_details(&error["details"]),
            Ok(()),
            "{code}: base details {}",
            error["details"]
        );
        let exit = base.code;
        assert_eq!(
            check_message(base, exit),
            "no panic",
            "a hand-built base passes"
        );
    }

    let mut planted: Vec<(&str, Run, Vec<&str>)> = Vec::new();
    let mut status = envelope(0);
    status["data"]["bogus"] = json!(1);
    planted.push((
        "data_key_missing_from_schema",
        rerun(0, &status, None),
        vec!["data /:", "unevaluatedProperties", "bogus"],
    ));
    let mut pinned = envelope(2);
    pinned["data"]["targets"][0]["bogus"] = json!(1);
    planted.push((
        "nested_data_key",
        rerun(2, &pinned, None),
        vec!["data /targets/0:", "bogus"],
    ));
    planted.push((
        "details_wrong_type",
        stale(json!("selection")),
        vec!["error /details/changed:", "type"],
    ));
    let mut missing = envelope(3);
    missing["error"]["details"]["extra"] = json!(1);
    planted.push((
        "details_undeclared_key",
        rerun(3, &missing, None),
        vec!["error /details:", "extra"],
    ));
    planted.push((
        "details_undeclared_reason",
        unsupported("bogus_reason"),
        vec!["error /details/reason:", "enum"],
    ));
    let mut warned = envelope(0);
    warned["warnings"] = json!([{"code": "reload_required", "message": "m", "fix": null, "fix_argv": null, "extra": true}]);
    planted.push((
        "warning_unknown_key",
        rerun(0, &warned, None),
        vec!["frame /warnings/0:", "extra"],
    ));
    let mut hinted = envelope(0);
    hinted["commands"] = json!([{"action": "undo", "command": "omaddons undo", "argv": ["omaddons", "undo"], "destructive": true, "extra": 1}]);
    planted.push((
        "command_hint_unknown_key",
        rerun(0, &hinted, None),
        vec!["frame /commands/0:", "extra"],
    ));
    planted.push((
        "stderr_event_unknown_field",
        rerun(
            0,
            &envelope(0),
            Some(r#"{"event":"log","level":"info","message":"m","x":1}"#),
        ),
        vec!["stderr line 1 /:", "'x'"],
    ));
    let mut target_error = envelope(2);
    target_error["data"]["targets"][0]["error"] = json!({
        "code": "plan_stale", "message": "m", "fix": "rerun the dry run", "fix_argv": null,
        "alternatives": [], "retryable": false, "details": {"changed": "selection"},
    });
    planted.push((
        "target_result_error_bad_details",
        rerun(2, &target_error, None),
        vec![
            "data /targets/0/error/details/changed:",
            "`plan_stale` error",
        ],
    ));

    for (name, run, expected) in planted {
        log.step(name);
        let planted: Value = serde_json::from_str(&run.stdout).unwrap();
        log.json("planted", &planted);
        log.note(format!("stderr: {}", run.stderr));
        let code = run.code;
        let message = check_message(run, code);
        log.note(&message);
        assert!(
            message.starts_with("schema validation failed for "),
            "case {name}: a schema failure, not an earlier check's:\n{message}"
        );
        for phrase in expected {
            assert!(
                message.contains(phrase),
                "case {name}: expected {phrase:?} in:\n{message}"
            );
        }
    }

    log.step("bases_still_pass");
    for (ran, code) in bases.drain(..) {
        support::check(ran.to_run(), code);
    }
}
