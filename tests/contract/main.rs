//! The M1 CLI contract conformance suite (vision §5.9, §7 M1 contract core): every
//! command, on success and on failure, prints exactly one JSON envelope on stdout with
//! the documented keys in order, an `exit_code` equal to the process exit and to the
//! error code's registered exit, fixes that are either an `omaddons` argv that
//! round-trips through shell-words or a documented non-CLI text, and NDJSON on stderr
//! with exactly one `error` event per failure. Plus the exit-site scan of `src/`.

#[path = "../common/mod.rs"]
mod common;

mod exit_sites;
mod matrix;
mod special;

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use common::archives::zip_text;
use common::home::{Home, Run};
use common::http::{Asset, gh_release};
use omaddons::error::ErrorCode;
use serde_json::Value;
use support::Scene;

#[cfg(test)]
mod support {
    use super::*;
    use serde::Deserialize;
    use serde::de::{IgnoredAny, MapAccess, Visitor};

    /// The envelope's keys in wire order.
    const ORDER: &[&str] = &[
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

    /// Fixes with no argv because they are not an omaddons command, or need input the
    /// suite cannot invent. Matched as prefixes; keep this list small and explicit.
    pub const TEXT_FIXES: &[&str] = &[
        "omaddons targets add <path>",
        "omaddons update <addon> --channel",
        "/reload in game",
        "restart the game client",
        "upgrade omaddons",
        "this folder belongs to the game client and is never modified",
        "check that the addon supports this client",
        "see data.items[].error",
        "run any omaddons command that changes addons",
        "wait for the running omaddons command to finish",
        "update the addon, or enable",
        "the AddOns filesystem does not support atomic renames",
        "use an archive built for this client",
        // adopt: the ref to fill in
        "omaddons adopt --ref <ref>",
        // missing_dependency
        "no keyless source ships",
        "install the one you use",
        "install it yourself",
        "install without --no-deps",
        "install a project that ships",
        "install the addon that ships",
        // desired_state_invalid and desired_unknown_key
        "use a family name",
        "keep one of the entries",
        "correct line",
        "remove it, or correct its spelling",
        // sync
        "pass the file",
        "check that",
        // stale_check (the WoWInterface index)
        "run the command again online",
    ];

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

    /// Asserts the whole contract on one `--json` run, and that it ended with `code`.
    pub fn check(run: Run, code: i32) -> Checked {
        let ctx = run.context();
        // Exactly one JSON document on stdout.
        let docs: Result<Vec<Value>, _> = serde_json::Deserializer::from_str(&run.stdout)
            .into_iter::<Value>()
            .collect();
        assert!(docs.is_ok(), "stdout is not JSON\n{ctx}");
        let docs = docs.unwrap();
        assert_eq!(docs.len(), 1, "exactly one envelope\n{ctx}");
        let envelope = docs.into_iter().next().unwrap();
        assert_eq!(run.code, code, "{ctx}");

        // Keys in wire order.
        let keys: Keys = serde_json::from_str(&run.stdout).unwrap();
        let expected: Vec<&str> = ORDER
            .iter()
            .copied()
            .filter(|k| keys.0.iter().any(|have| have == k))
            .collect();
        assert_eq!(keys.0, expected, "envelope key order\n{ctx}");

        // ok, exit_code and the registered exit agree.
        assert_eq!(envelope["schema_version"], "1.0", "{ctx}");
        assert_eq!(envelope["exit_code"], code, "{ctx}");
        assert_eq!(envelope["ok"], code == 0, "{ctx}");
        assert!(envelope["meta"]["elapsed_ms"].is_u64(), "{ctx}");
        assert!(envelope["meta"]["offline"].is_boolean(), "{ctx}");
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
            if code == 6 {
                assert!(
                    envelope.get("data").is_some(),
                    "check_failed keeps data\n{ctx}"
                );
            }
        }
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
            assert!(
                command["action"].is_string() && command["destructive"].is_boolean(),
                "{ctx}"
            );
        }

        // NDJSON on stderr, with exactly one error event per failure.
        let mut errors = 0;
        for line in run.stderr.lines() {
            let event = serde_json::from_str::<Value>(line);
            assert!(event.is_ok(), "stderr line is not JSON: {line}\n{ctx}");
            let event = event.unwrap();
            let kind = event["event"].as_str().unwrap_or_default();
            assert!(
                matches!(kind, "progress" | "log" | "warning" | "error"),
                "event {kind}\n{ctx}"
            );
            errors += usize::from(kind.eq("error"));
        }
        assert_eq!(
            errors,
            usize::from(code != 0),
            "error events on stderr\n{ctx}"
        );
        Checked { run, envelope }
    }

    /// A fix is non-empty; its argv starts with omaddons and joins back to the display,
    /// or it is null for a documented text fix.
    pub fn fix(display: &Value, argv: &Value, ctx: &str) {
        let display = display.as_str().unwrap_or_default();
        assert!(!display.is_empty(), "empty fix\n{ctx}");
        if argv.is_null() {
            assert!(
                TEXT_FIXES.iter().any(|t| display.starts_with(t))
                    || !display.starts_with("omaddons"),
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
                    } else {
                        scrub(v, root);
                    }
                }
            }
            _ => {}
        }
    }

    /// Replaces the tempdir root, ULIDs and RFC 3339 times in a string.
    fn redact(s: &str, root: &str) -> String {
        let s = s.replace(root, "<root>");
        if s.parse::<jiff::Timestamp>().is_ok() {
            return "<time>".to_owned();
        }
        let crockford =
            |c: char| c.is_ascii_digit() || (c.is_ascii_uppercase() && !"ILOU".contains(c));
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
    let two = r#"{"ok":true,"schema_version":"1.0","tool_version":"x","command":"t","exit_code":0,"data":{},"warnings":[],"commands":[],"meta":{"elapsed_ms":0,"offline":false}}"#;
    let doubled = format!("{two}\n{two}");
    assert!(
        std::panic::catch_unwind(|| support::check(run(&doubled, 0), 0)).is_err(),
        "two documents"
    );
    let empty_fix = r#"{"ok":false,"schema_version":"1.0","tool_version":"x","command":"t","exit_code":3,"error":{"code":"not_found","message":"m","fix":"","fix_argv":null,"alternatives":[],"retryable":false,"details":{}},"warnings":[],"commands":[],"meta":{"elapsed_ms":0,"offline":false}}"#;
    let mut failing = run(empty_fix, 3);
    failing.stderr = r#"{"event":"error"}"#.into();
    assert!(
        std::panic::catch_unwind(|| support::check(failing, 3)).is_err(),
        "empty fix"
    );
    let reordered = r#"{"schema_version":"1.0","ok":true,"tool_version":"x","command":"t","exit_code":0,"data":{},"warnings":[],"commands":[],"meta":{"elapsed_ms":0,"offline":false}}"#;
    assert!(
        std::panic::catch_unwind(|| support::check(run(reordered, 0), 0)).is_err(),
        "key order"
    );
    let ok = support::check(run(two, 0), 0);
    assert!(support::codes(&[&ok]).is_empty());
}
