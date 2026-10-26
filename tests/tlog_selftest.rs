//! e2e suite: the test log's own self-test (both files, every field, redaction, bounds, trees, the failure output).
//!
//! The structured test log (`tests/common/tlog.rs`) keeps its promises: its pure
//! helpers (secret names and values, path placeholders, caps, the test budget, names,
//! modes, runs, trees) case by case, the two files a log writes, read back after it is
//! dropped, and the `//! e2e suite:` convention `argc e2e` relies on. Subject logs are
//! written under the suite directory `tlog_selftest_child`, which is never a real suite,
//! so `argc e2e` never summarizes their deliberate failures.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

mod core_units {
    use std::fs;
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::path::{Path, PathBuf};

    use serde_json::{Value, json};

    use crate::common::tlog::{
        Budget, Caps, LOG_MODE_VAR, Mode, Relativizer, Scrubber, TestLog, Truncated, cap_text,
        is_secret_name, log_root, parse_mode, sanitize_component, split_location, tlog,
    };

    /// The suite directory of the subject logs.
    const CHILD: &str = "tlog_selftest_child";

    /// Every record of a dropped log's `steps.jsonl`, with its raw line.
    fn records(log: &Path) -> Vec<(String, Value)> {
        let text = fs::read_to_string(log.join("steps.jsonl")).unwrap();
        text.lines()
            .map(|line| {
                let value = serde_json::from_str::<Value>(line);
                assert!(
                    value.is_ok(),
                    "{}: a line that is no JSON: {line}",
                    log.display()
                );
                (line.to_owned(), value.unwrap())
            })
            .collect()
    }

    fn kinds(records: &[(String, Value)]) -> Vec<&str> {
        records
            .iter()
            .map(|(_, record)| record["kind"].as_str().unwrap())
            .collect()
    }

    fn of_kind<'a>(records: &'a [(String, Value)], kind: &str) -> Vec<&'a Value> {
        records
            .iter()
            .map(|(_, record)| record)
            .filter(|record| record["kind"] == kind)
            .collect()
    }

    fn transcript(log: &Path) -> String {
        fs::read_to_string(log.join("transcript.md")).unwrap()
    }

    /// The mode this test process logs in (`argc e2e` runs it `verbose`).
    fn this_mode() -> &'static str {
        let value = std::env::var(LOG_MODE_VAR).ok();
        parse_mode(value.as_deref()).unwrap().as_str()
    }

    /// Both files' bytes together.
    fn file_bytes(log: &Path) -> u64 {
        ["steps.jsonl", "transcript.md"]
            .iter()
            .map(|name| fs::metadata(log.join(name)).unwrap().len())
            .sum()
    }

    #[test]
    fn secret_names_are_recognized() {
        let cases = [
            ("omaddons_token", "OMADDONS_GITHUB_TOKEN", true),
            ("plain_token", "GITHUB_TOKEN", true),
            ("lower_case", "api_token", true),
            ("secret", "MY_SECRET", true),
            ("credential", "SOME_CREDENTIAL_FILE", true),
            ("fixtures", "OMADDONS_HTTP_FIXTURES", false),
            ("home", "HOME", false),
            ("failpoints", "OMADDONS_FAILPOINTS", false),
        ];
        for (name, input, expected) in cases {
            let output = is_secret_name(input);
            assert_eq!(output, expected, "{name}: input {input:?} gave {output:?}");
        }
    }

    #[test]
    fn secret_values_are_scrubbed_longest_first() {
        let cases: [(&str, &[&str], &str, &str); 7] = [
            (
                "twice",
                &["tlogSecret1"],
                "a tlogSecret1 b tlogSecret1 c",
                "a <redacted> b <redacted> c",
            ),
            (
                "edges",
                &["tlogSecret1"],
                "tlogSecret1 middle tlogSecret1",
                "<redacted> middle <redacted>",
            ),
            (
                "nested",
                &["abcdefgh", "abcdefgh-ijkl"],
                "x abcdefgh-ijkl y abcdefgh z",
                "x <redacted> y <redacted> z",
            ),
            (
                "nested_learned_longest_first",
                &["abcdefgh-ijkl", "abcdefgh"],
                "x abcdefgh-ijkl y abcdefgh z",
                "x <redacted> y <redacted> z",
            ),
            ("empty", &[""], "nothing to hide", "nothing to hide"),
            ("short", &["1234567"], "code 1234567", "code 1234567"),
            ("min", &["12345678"], "code 12345678", "code <redacted>"),
        ];
        for (name, secrets, input, expected) in cases {
            let mut scrubber = Scrubber::new();
            for secret in secrets {
                scrubber.add(secret);
            }
            let output = scrubber.scrub(input);
            assert_eq!(
                output, expected,
                "{name}: input {input:?} with secrets {secrets:?} gave {output:?}"
            );
        }
    }

    #[test]
    fn text_is_capped_on_a_char_boundary() {
        let cases = [
            ("under", "0123456789", 16, "0123456789", None),
            ("exact", "0123456789abcdef", 16, "0123456789abcdef", None),
            (
                "over",
                "0123456789abcdefg",
                16,
                "0123456789abcdef",
                Some(Truncated {
                    kept: 16,
                    total: 17,
                }),
            ),
            (
                "straddle",
                "0123456789abcde€",
                16,
                "0123456789abcde",
                Some(Truncated {
                    kept: 15,
                    total: 18,
                }),
            ),
        ];
        for (name, input, cap, kept, truncated) in cases {
            let output = cap_text(input, cap);
            assert_eq!(
                output,
                (kept, truncated),
                "{name}: input {input:?} capped at {cap} gave {output:?}"
            );
        }
    }

    #[test]
    fn the_test_budget_stops_writing_at_the_cap() {
        let mut budget = Budget::new(100);
        let steps = [(40, true), (40, true), (30, false), (1, false), (0, false)];
        for (i, (bytes, expected)) in steps.into_iter().enumerate() {
            let output = budget.admit(bytes);
            assert_eq!(
                output,
                expected,
                "record {i}: input {bytes} bytes after {} gave {output:?}",
                budget.used()
            );
        }
        assert_eq!(budget.used(), 80, "the refused records count no bytes");
        assert_eq!(budget.dropped(), 3, "every refused record is dropped");
    }

    #[test]
    fn function_paths_split_into_suite_and_test() {
        let cases = [
            (
                "root_fn",
                ("tlog_selftest", "tlog_selftest::x::__tlog_here"),
                ("tlog_selftest", "x"),
            ),
            (
                "nested",
                ("contract::matrix", "contract::matrix::row::__tlog_here"),
                ("contract", "matrix.row"),
            ),
            (
                "closure",
                (
                    "tlog_selftest::m",
                    "tlog_selftest::m::x::{{closure}}::{{closure}}::__tlog_here",
                ),
                ("tlog_selftest", "m.x"),
            ),
        ];
        for (name, (module_path, fn_path), (suite, test)) in cases {
            let output = split_location(module_path, fn_path);
            assert_eq!(
                output,
                (suite.to_owned(), test.to_owned()),
                "{name}: input {module_path:?}, {fn_path:?} gave {output:?}"
            );
        }
    }

    #[test]
    fn path_components_are_sanitized() {
        let cases = [
            ("space", "a b", "a_b"),
            ("slash", "a/b", "a_b"),
            ("dots", "..", "_"),
            ("dot", ".", "_"),
            ("empty", "", "_"),
            ("kept", "ok-name.1", "ok-name.1"),
        ];
        for (name, input, expected) in cases {
            let output = sanitize_component(input);
            assert_eq!(output, expected, "{name}: input {input:?} gave {output:?}");
        }
    }

    #[test]
    fn paths_are_rewritten_to_placeholders() {
        let target_tmp = Path::new(env!("CARGO_TARGET_TMPDIR"));
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
        let a = std::env::temp_dir().join("tlog-selftest-root-a");
        let b = std::env::temp_dir().join("tlog-selftest-root-b");
        let in_target = target_tmp.join("tlog-selftest-home");
        let show = |path: &Path, rest: &str| format!("{}{rest}", path.display());
        let cases: [(&str, Vec<&PathBuf>, String, String); 7] = [
            (
                "root",
                vec![&a],
                show(&a, "/home/x"),
                "<root>/home/x".into(),
            ),
            (
                "second_root",
                vec![&a, &b],
                show(&b, "/state"),
                "<root2>/state".into(),
            ),
            (
                "root_in_target_tmp",
                vec![&in_target],
                show(&in_target, "/home"),
                "<root>/home".into(),
            ),
            (
                "target_tmp",
                vec![],
                show(target_tmp, "/other"),
                "<target-tmp>/other".into(),
            ),
            ("repo", vec![], show(repo, "/src"), "<repo>/src".into()),
            (
                "other",
                vec![&a],
                "relative/path/x".into(),
                "relative/path/x".into(),
            ),
            (
                "sibling_is_not_the_root",
                vec![&a],
                show(&a, "-other/x"),
                show(&a, "-other/x"),
            ),
        ];
        for (name, roots, input, expected) in cases {
            let mut paths = Relativizer::new();
            for root in &roots {
                paths.add_root(root);
            }
            let output = paths.rewrite(&input);
            assert_eq!(output, expected, "{name}: input {input:?} gave {output:?}");
        }

        let mut paths = Relativizer::new();
        let first = paths.add_root(&a);
        let again = paths.add_root(&a);
        assert_eq!(
            (first.as_str(), again.as_str()),
            ("<root>", "<root>"),
            "a root added twice keeps its placeholder"
        );
        assert_eq!(paths.add_root(&b), "<root2>", "the next root is <root2>");
    }

    #[test]
    fn log_modes_parse() {
        let cases = [
            ("unset", None, Ok(Mode::Files)),
            ("empty", Some(""), Ok(Mode::Files)),
            ("verbose", Some("verbose"), Ok(Mode::Verbose)),
        ];
        for (name, input, expected) in cases {
            let output = parse_mode(input);
            assert_eq!(output, expected, "{name}: input {input:?} gave {output:?}");
        }
        let output = parse_mode(Some("verbos"));
        let message = output.as_ref().unwrap_err();
        assert!(
            message.contains("`verbose`") && message.contains("OMADDONS_TEST_LOG=verbos"),
            "typo: input \"verbos\" gave {output:?}"
        );
    }

    #[test]
    fn notes_and_json_write_both_files() {
        let log = TestLog::new(CHILD, "notes_and_json");
        let dir = log.dir().to_path_buf();
        log.step("first phase");
        log.note("the first note");
        log.json("sample", &json!({"a": 1}));
        log.assert_eq("count", &3, &3);
        drop(log);

        let records = records(&dir);
        let context = format!("subject {}", dir.join("transcript.md").display());
        assert_eq!(
            kinds(&records),
            ["header", "step", "note", "json", "assert", "end"],
            "{context}"
        );
        for (i, (line, record)) in records.iter().enumerate() {
            let prefix = format!(
                "{{\"kind\":\"{}\",\"seq\":{},",
                record["kind"].as_str().unwrap(),
                i + 1
            );
            assert!(
                line.starts_with(&prefix),
                "{context}: record {i} starts with kind then seq: {line}"
            );
        }
        let header = &records[0].1;
        assert_eq!(header["format"], 1, "{context}");
        assert_eq!(header["suite"], CHILD, "{context}");
        assert_eq!(header["test"], "notes_and_json", "{context}");
        assert_eq!(header["mode"], this_mode(), "{context}");
        assert_eq!(
            header["caps"],
            json!({"stream_bytes": 1 << 20, "test_bytes": 64u64 << 20}),
            "{context}"
        );
        assert_eq!(header["parent"], Value::Null, "{context}");
        assert_eq!(records[1].1["label"], "first phase", "{context}");
        assert_eq!(records[1].1["index"], 1, "{context}");
        assert_eq!(records[2].1["step"], "first phase", "{context}");
        assert_eq!(records[2].1["text"], "the first note", "{context}");
        assert_eq!(records[2].1["truncated"], Value::Null, "{context}");
        assert_eq!(records[3].1["value"], json!({"a": 1}), "{context}");
        assert_eq!(records[4].1["equal"], true, "{context}");
        let end = &records[5].1;
        assert_eq!(end["outcome"], "passed", "{context}");
        assert_eq!(end["records"], 6, "{context}");
        assert_eq!(end["runs"], 0, "{context}");
        assert_eq!(end["dropped"], 0, "{context}");

        let text = transcript(&dir);
        let mode_line = format!(" · mode {} · steps.jsonl beside this file\n", this_mode());
        for expected in [
            "# tlog_selftest_child :: notes_and_json\n",
            &mode_line,
            "\n## 1. first phase\n",
            "\nnote: the first note\n",
            "\n### json `sample`\n\n```json\n{\n  \"a\": 1\n}\n```\n",
            "\n### assert `count`: equal (3)\n",
            "\n---\n\nend: passed · 6 records · 0 runs · ",
        ] {
            assert!(
                text.contains(expected),
                "{context}: the transcript lacks {expected:?}:\n{text}"
            );
        }
        let bytes = end["bytes"].as_u64().unwrap();
        assert!(
            bytes < file_bytes(&dir),
            "{context}: end.bytes counts what came before it"
        );
    }

    #[test]
    fn a_rerun_replaces_the_directory() {
        let first = TestLog::new(CHILD, "rerun");
        let dir = first.dir().to_path_buf();
        first.note("from the first run");
        drop(first);
        fs::write(dir.join("planted.txt"), "left by an earlier run").unwrap();

        let second = TestLog::new(CHILD, "rerun");
        let context = format!("subject {}", second.transcript_path().display());
        assert!(
            !dir.join("planted.txt").exists(),
            "{context}: the planted file is gone"
        );
        assert_eq!(
            kinds(&records(&dir)),
            ["header"],
            "{context}: only the new header"
        );
        drop(second);
    }

    #[test]
    fn the_macro_names_the_suite_and_the_test() {
        let log = tlog!();
        let expected = Path::new("tlog_selftest/core_units.the_macro_names_the_suite_and_the_test");
        assert!(
            log.dir().starts_with(log_root()) && log.dir().ends_with(expected),
            "the macro named the directory {}",
            log.dir().display()
        );
        log.note("named by tlog!()");
    }

    #[test]
    fn an_oversized_json_value_is_truncated_and_says_so() {
        let caps = Caps {
            stream_bytes: 64,
            test_bytes: 64 << 10,
        };
        let log = TestLog::with_caps(CHILD, "oversized_json", caps);
        let dir = log.dir().to_path_buf();
        log.json("big", &"x".repeat(1024));
        drop(log);

        let records = records(&dir);
        let context = format!("subject {}", dir.join("transcript.md").display());
        let json = of_kind(&records, "json");
        assert_eq!(json.len(), 1, "{context}");
        assert_eq!(json[0]["value"], Value::Null, "{context}");
        assert_eq!(json[0]["text"].as_str().unwrap().len(), 64, "{context}");
        assert_eq!(
            json[0]["truncated"],
            json!({"kept": 64, "total": 1026}),
            "{context}"
        );
        assert!(
            transcript(&dir).contains("truncated: kept 64 of 1,026 bytes"),
            "{context}: the transcript says what was cut"
        );
    }

    #[test]
    fn a_long_note_is_truncated_and_says_so() {
        let caps = Caps {
            stream_bytes: 32,
            test_bytes: 64 << 10,
        };
        let log = TestLog::with_caps(CHILD, "long_note", caps);
        let dir = log.dir().to_path_buf();
        log.note("n".repeat(100));
        drop(log);

        let records = records(&dir);
        let context = format!("subject {}", dir.join("transcript.md").display());
        let note = of_kind(&records, "note");
        assert_eq!(note[0]["text"], "n".repeat(32), "{context}");
        assert_eq!(
            note[0]["truncated"],
            json!({"kept": 32, "total": 100}),
            "{context}"
        );
        assert!(
            transcript(&dir).contains("truncated: kept 32 of 100 bytes"),
            "{context}"
        );
    }

    #[test]
    fn the_cap_writes_one_capped_record() {
        let caps = Caps {
            stream_bytes: 1 << 10,
            test_bytes: 4 << 10,
        };
        let log = TestLog::with_caps(CHILD, "capped", caps);
        let dir = log.dir().to_path_buf();
        for i in 0..200 {
            log.note(format!("note {i:03}"));
        }
        drop(log);

        let records = records(&dir);
        let context = format!("subject {}", dir.join("transcript.md").display());
        assert_eq!(
            of_kind(&records, "capped").len(),
            1,
            "{context}: exactly one capped record"
        );
        assert_eq!(
            kinds(&records).last(),
            Some(&"end"),
            "{context}: the end record follows the cap"
        );
        let written = of_kind(&records, "note").len();
        assert!(
            written > 0 && written < 200,
            "{context}: {written} notes written"
        );
        let end = of_kind(&records, "end")[0];
        assert_eq!(
            end["dropped"],
            200 - written as u64,
            "{context}: every note left out is counted"
        );
        assert_eq!(end["records"], records.len() as u64, "{context}");

        let text = transcript(&dir);
        assert!(
            text.contains("log capped at 4,096 bytes: the records after this one are not written"),
            "{context}: the transcript says the log was capped"
        );
        let end_line = records.last().unwrap().0.len() as u64 + 1;
        let end_part = (text.len() - text.rfind("\n---\n\nend: ").unwrap()) as u64;
        let before_end = file_bytes(&dir) - end_line - end_part;
        assert!(
            before_end <= 4 << 10,
            "{context}: {before_end} bytes before the end record, past the 4 KiB cap"
        );
        assert_eq!(
            end["bytes"], before_end,
            "{context}: end.bytes is what came before it"
        );
    }

    #[test]
    fn a_registered_value_is_scrubbed_from_every_record() {
        // ubs:ignore — a fake planted value; the test asserts it never reaches the log
        let planted = "tlogSelftestValue-0123";
        let log = TestLog::new(CHILD, "registered_value");
        let dir = log.dir().to_path_buf();
        log.secret(planted);
        log.step(&format!("use {planted}"));
        log.note(format!("first {planted} then {planted}"));
        log.json("value", &json!({"field": planted, planted: "as a key"}));
        drop(log);

        let records = records(&dir);
        let context = format!("subject {}", dir.join("transcript.md").display());
        for name in ["steps.jsonl", "transcript.md"] {
            let text = fs::read_to_string(dir.join(name)).unwrap();
            assert!(
                !text.contains(planted),
                "{context}: {name} holds the planted value"
            );
        }
        assert_eq!(
            of_kind(&records, "step")[0]["label"],
            "use <redacted>",
            "{context}"
        );
        assert_eq!(
            of_kind(&records, "note")[0]["text"],
            "first <redacted> then <redacted>",
            "{context}"
        );
        assert_eq!(
            of_kind(&records, "json")[0]["value"],
            json!({"field": "<redacted>", "<redacted>": "as a key"}),
            "{context}"
        );
    }

    #[test]
    fn the_repo_and_the_target_directory_are_placeholders() {
        let repo = env!("CARGO_MANIFEST_DIR");
        let target_tmp = env!("CARGO_TARGET_TMPDIR");
        let log = TestLog::new(CHILD, "placeholders");
        let dir = log.dir().to_path_buf();
        log.note(format!("read {repo}/src and wrote {target_tmp}/x"));
        log.json("paths", &json!([format!("{repo}/README.md"), target_tmp]));
        drop(log);

        let records = records(&dir);
        let context = format!("subject {}", dir.join("transcript.md").display());
        assert_eq!(
            of_kind(&records, "note")[0]["text"],
            "read <repo>/src and wrote <target-tmp>/x",
            "{context}"
        );
        assert_eq!(
            of_kind(&records, "json")[0]["value"],
            json!(["<repo>/README.md", "<target-tmp>"]),
            "{context}"
        );
        for name in ["steps.jsonl", "transcript.md"] {
            let text = fs::read_to_string(dir.join(name)).unwrap();
            assert!(
                !text.contains(repo) && !text.contains(target_tmp),
                "{context}: {name} holds an absolute path"
            );
        }
    }

    #[test]
    fn assert_eq_failure_names_the_step_and_the_transcript() {
        let log = TestLog::new(CHILD, "assert_eq_failure");
        let dir = log.dir().to_path_buf();
        log.step("compare the counts");
        let caught = catch_unwind(AssertUnwindSafe(|| log.assert_eq("count", &2, &3)));
        let payload = caught.expect_err("a mismatch panics");
        let message = payload.downcast_ref::<String>().unwrap();
        let context = format!("subject {}", log.transcript_path().display());
        for expected in [
            "tlog: assert \"count\" in step \"compare the counts\": expected 3, got 2",
            "\n  transcript: ",
            &log.transcript_path().display().to_string(),
        ] {
            assert!(
                message.contains(expected),
                "{context}: the panic lacks {expected:?}: {message}"
            );
        }
        drop(log);

        let records = records(&dir);
        assert_eq!(
            kinds(&records),
            ["header", "step", "assert", "failure", "end"],
            "{context}"
        );
        let assert = of_kind(&records, "assert")[0];
        assert_eq!(
            (&assert["actual"], &assert["expected"]),
            (&json!(2), &json!(3)),
            "{context}"
        );
        assert_eq!(assert["equal"], false, "{context}");
        let failure = of_kind(&records, "failure")[0];
        assert_eq!(failure["step"], "compare the counts", "{context}");
        assert!(
            !failure["message"].as_str().unwrap().contains("transcript:"),
            "{context}: the transcript's absolute path stays out of the files"
        );
        assert!(
            transcript(&dir).contains("### assert `count`: differs"),
            "{context}: the transcript shows the mismatch"
        );
    }

    #[test]
    fn threads_share_one_log() {
        fn send_sync<T: Send + Sync>() {}
        send_sync::<TestLog>();

        let log = TestLog::new(CHILD, "threads");
        let dir = log.dir().to_path_buf();
        std::thread::scope(|scope| {
            for thread in 0..4 {
                let log = &log;
                scope.spawn(move || {
                    for note in 0..10 {
                        log.note(format!("thread {thread} note {note}"));
                    }
                });
            }
        });
        drop(log);

        let records = records(&dir);
        let context = format!("subject {}", dir.join("transcript.md").display());
        assert_eq!(of_kind(&records, "note").len(), 40, "{context}");
        let seqs: Vec<u64> = records
            .iter()
            .map(|(_, r)| r["seq"].as_u64().unwrap())
            .collect();
        let expected: Vec<u64> = (1..=42).collect();
        assert_eq!(seqs, expected, "{context}: one sequence, in file order");
    }
}

mod run_units {
    use std::collections::BTreeMap;
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::path::Path;
    use std::process::Command;

    use serde_json::{Value, json};

    use crate::common::env::TestEnv;
    use crate::common::home::Home;
    use crate::common::tlog::{
        NetLogAdded, Runner, Stdout, TestLog, base_vars, classify_stdout, net_log_added, overrides,
        parse_stderr, root_allowed,
    };
    use crate::common::trees::{EntryKind, listing, tree_diff, tree_entries, tree_hash};

    /// The suite directory of the subject logs.
    const CHILD: &str = "tlog_selftest_child";

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    #[test]
    fn stdout_is_classified() {
        let cases = [
            ("envelope", "{\"ok\":true}\n", "envelope"),
            ("two_lines", "{\"ok\":true}\nx\n", "text"),
            ("plain", "omaddons 0.1.0\n", "text"),
            ("empty", "", "empty"),
            ("array", "[1]\n", "text"),
            ("no_newline", "{\"ok\":false}", "envelope"),
        ];
        for (name, input, expected) in cases {
            let output = classify_stdout(input, 1 << 20);
            let form = match &output {
                Stdout::Envelope { .. } => "envelope",
                Stdout::Text { .. } => "text",
                Stdout::Empty { .. } => "empty",
            };
            assert_eq!(form, expected, "{name}: input {input:?} gave {output:?}");
        }
        let input = "{\"ok\":true,\"data\":\"0123456789\"}\n";
        let output = classify_stdout(input, 8);
        let cut = matches!(
            &output,
            Stdout::Text { text, truncated: Some(t), .. } if text == "{\"ok\":tr" && t.kept == 8
        );
        assert!(cut, "truncated_envelope: input {input:?} gave {output:?}");
    }

    #[test]
    fn stderr_lines_parse_one_by_one() {
        let event = "{\"event\":\"log\",\"level\":\"debug\"}";
        let parsed = json!({"event": "log", "level": "debug"});
        let cases: [(&str, String, Vec<Value>); 3] = [
            (
                "mixed",
                format!("{event}\nplain text\n{event}\n"),
                vec![parsed.clone(), json!("plain text"), parsed.clone()],
            ),
            ("empty", String::new(), vec![]),
            (
                "blank_line",
                format!("{event}\n\n{event}\n"),
                vec![parsed.clone(), json!(""), parsed.clone()],
            ),
        ];
        for (name, input, expected) in cases {
            let output = parse_stderr(&input, 1 << 20);
            assert_eq!(
                output.lines, expected,
                "{name}: input {input:?} gave {output:?}"
            );
            assert_eq!(
                output.bytes,
                input.len(),
                "{name}: input {input:?} gave {output:?}"
            );
        }
    }

    #[test]
    fn net_log_additions_are_the_new_suffix() {
        let (a, b, c) = (
            "{\"purpose\":\"a\"}",
            "{\"purpose\":\"b\"}",
            "{\"purpose\":\"c\"}",
        );
        let added = |lines: &[&str], rotated| NetLogAdded {
            added: lines
                .iter()
                .map(|line| serde_json::from_str(line).unwrap_or_else(|_| json!(line)))
                .collect(),
            rotated,
        };
        // (name, before: net.log.1 then net.log, after: net.log.1, after: net.log, expected)
        let cases = [
            (
                "appended",
                strings(&[a]),
                strings(&[]),
                strings(&[a, b, c]),
                added(&[b, c], false),
            ),
            (
                "first_run",
                strings(&[]),
                strings(&[]),
                strings(&[a, b]),
                added(&[a, b], false),
            ),
            (
                "nothing",
                strings(&[a]),
                strings(&[]),
                strings(&[a]),
                added(&[], false),
            ),
            (
                "rotation_kept",
                strings(&[a, b]),
                strings(&[a, b]),
                strings(&[c]),
                added(&[c], false),
            ),
            (
                "rotation_dropped",
                strings(&[a, b]),
                strings(&[b]),
                strings(&[c]),
                added(&[c], true),
            ),
            (
                "garbage",
                strings(&[]),
                strings(&[]),
                strings(&["not json"]),
                added(&["not json"], false),
            ),
        ];
        for (name, before, after_old, after_current, expected) in cases {
            let output = net_log_added(&before, &after_old, &after_current);
            assert_eq!(
                output, expected,
                "{name}: input {before:?} then {after_old:?} + {after_current:?} gave {output:?}"
            );
        }
    }

    #[test]
    fn overrides_exclude_the_base_and_redact_secrets() {
        let env = TestEnv::new();
        let base = base_vars(&env);
        let home = Home::new(&["wow"]);
        let home_base = base_vars(&home.env);
        let argv = strings(&["status"]);

        let plain = Runner::command(&env, &argv);
        let mut proc_root = Runner::command(&env, &argv);
        let other_proc = env.root().join("other-proc");
        proc_root.env("OMADDONS_PROC_ROOT", &other_proc);
        let fixtures = Runner::command(&home, &argv);
        let mut token = Runner::command(&env, &argv);
        // ubs:ignore — a fake planted value; the test asserts it is redacted
        token.env("OMADDONS_GITHUB_TOKEN", "tlogSelftestFake-0123");
        let mut removed = Runner::command(&env, &argv);
        removed.env_remove("HOME");

        // (name, the environment's base, the command, the overrides expected)
        type Case<'a> = (
            &'a str,
            &'a BTreeMap<String, String>,
            &'a Command,
            Vec<(&'a str, String)>,
        );
        let cases: [Case<'_>; 5] = [
            ("base_only", &base, &plain, vec![]),
            (
                "proc_root_changed",
                &base,
                &proc_root,
                vec![("OMADDONS_PROC_ROOT", other_proc.display().to_string())],
            ),
            (
                "fixtures",
                &home_base,
                &fixtures,
                vec![("OMADDONS_HTTP_FIXTURES", home.http.display().to_string())],
            ),
            (
                "token",
                &base,
                &token,
                vec![("OMADDONS_GITHUB_TOKEN", "<redacted>".to_owned())],
            ),
            ("removed", &base, &removed, vec![]),
        ];
        for (name, base, command, expected) in cases {
            let output = overrides(base, command);
            let expected: BTreeMap<String, String> = expected
                .into_iter()
                .map(|(key, value)| (key.to_owned(), value))
                .collect();
            assert_eq!(output.vars, expected, "{name}: {command:?} gave {output:?}");
        }
        let output = overrides(&base, &token);
        let learned: Vec<&str> = output
            .secrets
            .iter()
            .map(|(name, _)| name.as_str())
            .collect();
        assert_eq!(learned, ["OMADDONS_GITHUB_TOKEN"], "token: gave {output:?}");
    }

    /// A tree: `a.txt`, `dir/b.txt`, an empty `dir/empty/`, and `link` → `a.txt`.
    fn sample_tree(root: &Path) {
        fs::create_dir_all(root.join("dir/empty")).unwrap();
        fs::write(root.join("a.txt"), "alpha").unwrap();
        fs::write(root.join("dir/b.txt"), "bravo").unwrap();
        symlink("a.txt", root.join("link")).unwrap();
    }

    #[test]
    fn tree_entries_cover_kind_mode_content_and_target() {
        let env = TestEnv::new();
        let root = env.root().join("tree");
        sample_tree(&root);
        fs::write(root.join("private.txt"), "0600").unwrap();
        fs::set_permissions(root.join("private.txt"), fs::Permissions::from_mode(0o600)).unwrap();
        let entries = tree_entries(&root);
        let context = listing(&root);
        let paths: Vec<&str> = entries.keys().map(String::as_str).collect();
        assert_eq!(
            paths,
            [
                "a.txt",
                "dir",
                "dir/b.txt",
                "dir/empty",
                "link",
                "private.txt"
            ],
            "{context}"
        );
        let a = &entries["a.txt"];
        assert_eq!(
            (a.kind, a.len, a.target.as_deref()),
            (EntryKind::File, 5, None),
            "{context}"
        );
        assert_eq!(
            a.sha256.as_deref(),
            Some("8ed3f6ad685b959ead7022518e1af76cd816f8e8ec7ccdda1ed4018e8f2223f8"),
            "{context}: the SHA-256 of `alpha`"
        );
        assert_eq!(entries["dir"].kind, EntryKind::Dir, "{context}");
        assert_eq!(entries["dir/empty"].kind, EntryKind::Dir, "{context}");
        let link = &entries["link"];
        assert_eq!(
            (link.kind, link.target.as_deref(), link.sha256.as_deref()),
            (EntryKind::Symlink, Some("a.txt"), None),
            "{context}"
        );
        assert_eq!(entries["private.txt"].mode, 0o600, "{context}");
    }

    type Mutate = fn(&Path);

    /// Each change to [`sample_tree`] and the lines `TreeDiff::first` gives for it.
    fn tree_changes() -> [(&'static str, Mutate, &'static [&'static str]); 7] {
        [
            (
                "added",
                |r| fs::write(r.join("new.txt"), "n").unwrap(),
                &["+ new.txt"],
            ),
            (
                "removed",
                |r| fs::remove_file(r.join("dir/b.txt")).unwrap(),
                &["- dir/b.txt"],
            ),
            (
                "same_size_edit",
                |r| fs::write(r.join("a.txt"), "alphA").unwrap(),
                &["~ a.txt (content)"],
            ),
            (
                "chmod",
                |r| {
                    fs::set_permissions(r.join("a.txt"), fs::Permissions::from_mode(0o600)).unwrap()
                },
                &["~ a.txt (mode)"],
            ),
            (
                "file_to_dir",
                |r| {
                    fs::remove_file(r.join("a.txt")).unwrap();
                    fs::create_dir(r.join("a.txt")).unwrap();
                },
                &["~ a.txt (kind)"],
            ),
            (
                "retarget",
                |r| {
                    fs::remove_file(r.join("link")).unwrap();
                    symlink("dir/b.txt", r.join("link")).unwrap();
                },
                &["~ link (target)"],
            ),
            (
                "empty_dir",
                |r| fs::create_dir(r.join("dir/also-empty")).unwrap(),
                &["+ dir/also-empty"],
            ),
        ]
    }

    #[test]
    fn tree_diff_names_what_changed() {
        for (name, mutate, expected) in tree_changes() {
            let env = TestEnv::new();
            let root = env.root().join("tree");
            sample_tree(&root);
            let before = tree_entries(&root);
            mutate(&root);
            let diff = tree_diff(&before, &tree_entries(&root));
            let output = diff.first(20);
            assert_eq!(
                output,
                expected,
                "{name}: gave {diff:?}\n{}",
                listing(&root)
            );
            assert_eq!(diff.total, expected.len(), "{name}: gave {diff:?}");
        }

        let env = TestEnv::new();
        let root = env.root().join("many");
        fs::create_dir_all(&root).unwrap();
        for i in 0..25 {
            fs::write(root.join(format!("f{i:02}")), "x").unwrap();
        }
        let before = tree_entries(&root);
        for i in 0..25 {
            fs::write(root.join(format!("f{i:02}")), "y").unwrap();
        }
        let diff = tree_diff(&before, &tree_entries(&root));
        let expected: Vec<String> = (0..20).map(|i| format!("~ f{i:02} (content)")).collect();
        assert_eq!(diff.total, 25, "many: gave {diff:?}");
        assert_eq!(diff.first(20), expected, "many: the first 20 in path order");
    }

    #[test]
    fn tree_hash_and_tree_entries_agree() {
        let unchanged: Mutate = |_| {};
        let cases = tree_changes()
            .into_iter()
            .map(|(name, mutate, _)| (name, mutate))
            .chain([("unchanged", unchanged)]);
        for (name, mutate) in cases {
            let env = TestEnv::new();
            let root = env.root().join("tree");
            sample_tree(&root);
            let (hash, entries) = (tree_hash(&root), tree_entries(&root));
            mutate(&root);
            let hash_moved = hash != tree_hash(&root);
            let diff = tree_diff(&entries, &tree_entries(&root));
            assert_eq!(
                hash_moved,
                !diff.is_empty(),
                "{name}: tree_hash moved {hash_moved}, tree_diff gave {diff:?}\n{}",
                listing(&root)
            );
        }
    }

    #[test]
    fn tree_roots_outside_the_test_are_refused() {
        let env = TestEnv::new();
        let roots = [env.root().to_path_buf()];
        let cases = [
            ("env_root", env.root().join("home"), true),
            (
                "target_tmp",
                Path::new(env!("CARGO_TARGET_TMPDIR")).join("tlog-selftest-tree"),
                true,
            ),
            (
                "repo",
                Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
                false,
            ),
            (
                "parent_escape",
                env.root().join("../../no-such-dir/x"),
                false,
            ),
        ];
        for (name, input, expected) in cases {
            let output = root_allowed(&input, &roots);
            assert_eq!(output, expected, "{name}: input {input:?} gave {output:?}");
        }

        let log = TestLog::new(CHILD, "refused_root");
        let dir = log.dir().to_path_buf();
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let caught = catch_unwind(AssertUnwindSafe(|| log.tree("repo", &repo)));
        let payload = caught.expect_err("a root in the repository is refused");
        let message = payload.downcast_ref::<String>().unwrap();
        assert!(
            message.contains("is outside this test's synthetic directories"),
            "refused: the panic says why: {message}"
        );
        drop(log);
        let steps = fs::read_to_string(dir.join("steps.jsonl")).unwrap();
        assert!(
            !steps.contains("\"kind\":\"tree\"") && steps.contains("\"kind\":\"failure\""),
            "refused: no tree was recorded, a failure was: {steps}"
        );
    }
}

/// The `//! e2e suite:` marker tells `argc e2e` which test crates are end-to-end suites;
/// these tests keep it honest, from source text alone (no build, no process).
mod convention {
    use std::fs;
    use std::path::{Path, PathBuf};

    use crate::common::tlog::E2E_MARKER;

    /// One test crate's source: its name, its root file's text, and the texts of its
    /// other files (a directory crate's modules).
    struct CrateText {
        name: String,
        root: String,
        others: Vec<String>,
    }

    /// A line that is not a comment calls `tlog!(` or `TestLog::`.
    fn uses_the_log(text: &str) -> bool {
        text.lines().any(|line| {
            let line = line.trim();
            !line.starts_with("//") && (line.contains("tlog!(") || line.contains("TestLog::"))
        })
    }

    /// The root file has a line that starts with the marker.
    fn is_marked(root: &str) -> bool {
        root.lines().any(|line| line.starts_with(E2E_MARKER))
    }

    /// Every crate that breaks the convention, with its fix: a crate that logs through
    /// `TestLog` carries the marker, a marked crate logs through it, and every `e2e_*`
    /// crate is marked.
    fn convention_offenders(crates: &[CrateText]) -> Vec<String> {
        let mut offenders = Vec::new();
        for krate in crates {
            let used = uses_the_log(&krate.root) || krate.others.iter().any(|t| uses_the_log(t));
            let marked = is_marked(&krate.root);
            let name = &krate.name;
            if used && !marked {
                offenders.push(format!(
                    "{name}: logs through TestLog without the marker: add `{E2E_MARKER} <purpose>` to its crate doc"
                ));
            } else if marked && !used {
                offenders.push(format!(
                    "{name}: carries the marker but never logs: remove the marker, or log through `TestLog`"
                ));
            } else if name.starts_with("e2e_") && !marked {
                offenders.push(format!(
                    "{name}: an e2e_ crate without the marker: add `{E2E_MARKER} <purpose>` to its crate doc"
                ));
            }
        }
        offenders
    }

    fn rs_files(dir: &Path) -> Vec<PathBuf> {
        let mut files = Vec::new();
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                files.extend(rs_files(&path));
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                files.push(path);
            }
        }
        files.sort();
        files
    }

    /// The repository's test crates: each `tests/<name>.rs`, and each
    /// `tests/<name>/main.rs` with every other `.rs` file under `tests/<name>/`.
    fn test_crates() -> Vec<CrateText> {
        let tests = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
        let mut crates = Vec::new();
        for entry in fs::read_dir(&tests).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_stem().unwrap().to_string_lossy().into_owned();
            if path.is_dir() {
                let main = path.join("main.rs");
                if !main.is_file() {
                    continue;
                }
                let others = rs_files(&path)
                    .into_iter()
                    .filter(|file| *file != main)
                    .map(|file| fs::read_to_string(file).unwrap())
                    .collect();
                let root = fs::read_to_string(&main).unwrap();
                crates.push(CrateText { name, root, others });
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                let root = fs::read_to_string(&path).unwrap();
                crates.push(CrateText {
                    name,
                    root,
                    others: Vec::new(),
                });
            }
        }
        crates.sort_by(|a, b| a.name.cmp(&b.name));
        crates
    }

    #[test]
    fn the_convention_check_finds_each_kind_of_offender() {
        let marker = format!("{E2E_MARKER} a suite\n");
        let crate_text = |name: &str, root: &str, others: &[&str]| CrateText {
            name: name.to_owned(),
            root: root.to_owned(),
            others: others.iter().map(|text| (*text).to_owned()).collect(),
        };
        let cases = [
            (
                "marked_and_used",
                crate_text(
                    "suite",
                    &format!("{marker}fn x() {{ let log = tlog!(); }}\n"),
                    &[],
                ),
                None,
            ),
            (
                "used_unmarked",
                crate_text(
                    "suite",
                    "fn x() { let log = TestLog::new(\"a\", \"b\"); }\n",
                    &[],
                ),
                Some("add `//! e2e suite: <purpose>`"),
            ),
            (
                "marked_unused",
                crate_text("suite", &marker, &[]),
                Some("remove the marker, or log through `TestLog`"),
            ),
            (
                "e2e_unmarked",
                crate_text("e2e_slice9", "fn x() {}\n", &[]),
                Some("an e2e_ crate without the marker"),
            ),
            (
                "comment_only",
                crate_text("suite", "// see tlog!() in tests/common\nfn x() {}\n", &[]),
                None,
            ),
            (
                "dir_crate_submodule",
                crate_text("suite", &marker, &["fn y() { let log = tlog!(); }\n"]),
                None,
            ),
            (
                "marker_not_at_line_start",
                crate_text(
                    "suite",
                    "const TEXT: &str = \"\n    //! e2e suite: inside a string\";\nfn x() { let log = tlog!(); }\n",
                    &[],
                ),
                Some("add `//! e2e suite: <purpose>`"),
            ),
        ];
        for (name, krate, expected) in cases {
            let input = format!("root {:?}, others {:?}", krate.root, krate.others);
            let output = convention_offenders(std::slice::from_ref(&krate));
            match expected {
                None => assert!(output.is_empty(), "{name}: input {input} gave {output:?}"),
                Some(fix) => assert!(
                    output.len() == 1 && output[0].contains(fix),
                    "{name}: input {input} gave {output:?}"
                ),
            }
        }
    }

    #[test]
    fn every_test_log_suite_carries_the_e2e_marker() {
        let crates = test_crates();
        assert!(
            crates.iter().any(|krate| krate.name == "tlog_selftest"),
            "the scan found this crate"
        );
        let offenders = convention_offenders(&crates);
        assert!(
            offenders.is_empty(),
            "test crates that break the e2e convention:\n{}",
            offenders.join("\n")
        );
    }

    #[test]
    fn the_runner_uses_the_marker() {
        let argcfile = Path::new(env!("CARGO_MANIFEST_DIR")).join("Argcfile.sh");
        let text = fs::read_to_string(&argcfile).unwrap();
        let pattern = format!("'^{E2E_MARKER}'");
        assert!(
            text.contains("\ne2e() {") && text.contains(&pattern),
            "Argcfile.sh has an e2e task that greps {pattern}, so the runner and E2E_MARKER agree"
        );
    }
}

/// The log's promises over the real binary: each scenario logs itself through `tlog!()`
/// and inspects a subject log under `tlog_selftest_child`, read after it is dropped.
mod scenarios {
    use std::any::Any;
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    use serde_json::{Value, json};

    use crate::common::addons::toc;
    use crate::common::engine::Scene;
    use crate::common::env::TestEnv;
    use crate::common::home::Home;
    use crate::common::http::{Asset, gh_release, paths};
    use crate::common::netlog::read_net_log;
    use crate::common::pause::Pause;
    use crate::common::tlog::{Caps, TestLog, tlog};

    /// The suite directory of the subject logs.
    const CHILD: &str = "tlog_selftest_child";
    /// Every field a `run` record carries.
    const RUN_FIELDS: &[&str] = &[
        "seq",
        "step",
        "env",
        "argv",
        "cwd",
        "overrides",
        "failpoints",
        "stdin_bytes",
        "background",
        "started_at",
        "elapsed_ms",
        "exit",
        "signal",
        "timed_out",
        "stdout",
        "stderr",
        "net_log",
        "scrubbed",
    ];

    /// A dropped subject log's records, in file order.
    fn records(dir: &Path) -> Vec<Value> {
        let text = fs::read_to_string(dir.join("steps.jsonl")).unwrap();
        text.lines()
            .enumerate()
            .map(|(i, line)| {
                let record = serde_json::from_str::<Value>(line);
                assert!(
                    record.is_ok(),
                    "{}: steps.jsonl line {} is no JSON: {line}",
                    dir.display(),
                    i + 1
                );
                record.unwrap()
            })
            .collect()
    }

    fn kinds(records: &[Value]) -> Vec<&str> {
        records
            .iter()
            .map(|r| r["kind"].as_str().unwrap())
            .collect()
    }

    fn of_kind<'a>(records: &'a [Value], kind: &str) -> Vec<&'a Value> {
        // ubs:ignore — comparing record kinds, not secrets
        records.iter().filter(|r| r["kind"] == kind).collect()
    }

    fn file(dir: &Path, name: &str) -> String {
        fs::read_to_string(dir.join(name)).unwrap()
    }

    /// Both files' bytes together.
    fn bytes(dir: &Path) -> u64 {
        ["steps.jsonl", "transcript.md"]
            .iter()
            .map(|name| fs::metadata(dir.join(name)).unwrap().len())
            .sum()
    }

    /// A panic's message.
    fn message(payload: Box<dyn Any + Send>) -> String {
        match payload.downcast::<String>() {
            Ok(text) => *text,
            Err(payload) => payload
                .downcast_ref::<&str>()
                .map(|text| (*text).to_owned())
                .unwrap_or_default(),
        }
    }

    /// The subject's records, with the scenario's own log saying what it reads.
    fn read(log: &TestLog, dir: &Path) -> Vec<Value> {
        log.step("read its files");
        log.note(format!(
            "subject transcript: {}",
            dir.join("transcript.md").display()
        ));
        let records = records(dir);
        log.json("subject kinds", &kinds(&records));
        log.step("check");
        records
    }

    #[test]
    fn a_passing_scenario_writes_both_files() {
        let log = tlog!();
        log.step("build the subject");
        let env = TestEnv::new();
        let subject = TestLog::new(CHILD, "a_passing_scenario");
        let dir = subject.dir().to_path_buf();

        log.step("run it");
        subject.step("help, then a usage error");
        subject.run(&env, &["help", "--json"]).ok();
        subject
            .run(&env, &["frobnicate", "--json"])
            .fails(2, "usage");
        subject.step("status");
        subject.run(&env, &["status", "--json"]).ok();
        subject.tree("state", &env.state());
        subject
            .run(&env, &["config", "get", "check_interval_hours", "--json"])
            .ok();
        subject.assert_unchanged("state", &env.state());
        drop(subject);

        let records = read(&log, &dir);
        let context = format!("subject {}", dir.join("transcript.md").display());
        log.assert_eq(
            "kinds",
            &kinds(&records),
            &vec![
                "header",
                "step",
                "env",
                "run",
                "run",
                "step",
                "run",
                "tree",
                "run",
                "tree_check",
                "end",
            ],
        );
        let runs = of_kind(&records, "run");
        for (i, run) in runs.iter().enumerate() {
            let missing: Vec<&&str> = RUN_FIELDS
                .iter()
                .filter(|field| run.get(**field).is_none())
                .collect();
            assert!(
                missing.is_empty(),
                "{context}: run {i} lacks {missing:?}: {run}"
            );
            assert_eq!(run["argv"][0], "omaddons", "{context}: run {i}");
            assert_eq!(run["env"], "<root>", "{context}: run {i}");
            assert_eq!(run["cwd"], "<repo>", "{context}: run {i}");
            assert_eq!(run["overrides"], json!({}), "{context}: run {i}");
            assert_eq!(run["failpoints"], Value::Null, "{context}: run {i}");
            assert_eq!(run["stdin_bytes"], Value::Null, "{context}: run {i}");
            assert_eq!(run["background"], false, "{context}: run {i}");
            assert_eq!(run["timed_out"], false, "{context}: run {i}");
            assert_eq!(run["signal"], Value::Null, "{context}: run {i}");
            assert!(run["elapsed_ms"].is_u64(), "{context}: run {i}");
            let started = run["started_at"].as_str().unwrap();
            assert!(
                started.parse::<jiff::Timestamp>().is_ok(),
                "{context}: run {i}: {started}"
            );
            assert_eq!(
                run["net_log"],
                json!({"added": [], "rotated": false}),
                "{context}: run {i}"
            );
            assert_eq!(run["scrubbed"], json!([]), "{context}: run {i}");
        }
        assert_eq!(
            runs[0]["argv"],
            json!(["omaddons", "help", "--json"]),
            "{context}"
        );
        assert_eq!(runs[0]["stdout"]["form"], "envelope", "{context}");
        assert_eq!(runs[0]["stdout"]["value"]["ok"], true, "{context}");
        assert_eq!(runs[1]["exit"], 2, "{context}");
        assert_eq!(
            runs[1]["stdout"]["value"]["error"]["code"], "usage",
            "{context}"
        );
        assert_eq!(runs[1]["stderr"]["lines"][0]["event"], "error", "{context}");
        let end = records.last().unwrap();
        assert_eq!(
            (&end["outcome"], &end["runs"]),
            (&json!("passed"), &json!(4)),
            "{context}"
        );

        let transcript = file(&dir, "transcript.md");
        for expected in [
            "\n## 1. help, then a usage error\n",
            "\n## 2. status\n",
            " run `omaddons help --json` → exit 0 (",
            " run `omaddons frobnicate --json` → exit 2 (",
            "stdout (envelope):\n\n```json\n{\n",
            "tree check `state`: unchanged",
        ] {
            assert!(
                transcript.contains(expected),
                "{context}: the transcript lacks {expected:?}"
            );
        }
        let size = bytes(&dir);
        log.json("subject bytes", &size);
        assert!(size < 64 << 10, "{context}: {size} bytes, past 64 KiB");
    }

    #[test]
    fn net_log_lines_are_recorded() {
        const MAINLINE: &[(&str, u32)] = &[("mainline", 120_105)];
        let log = tlog!();
        log.step("build the subject");
        let home = Home::new(&["wow"]);
        let text = toc("120105", &["## Title: Foo", "## Version: 1.0"]);
        let assets: [Asset<'_>; 1] = [(
            "Foo.zip",
            MAINLINE,
            vec![("Foo/Foo.toc", text.as_str()), ("Foo/core.lua", "1.0")],
        )];
        gh_release(&home.http, "https://github.com/o/foo", "v1", &assets);
        let subject = TestLog::new(CHILD, "net_log_lines");
        let dir = subject.dir().to_path_buf();

        log.step("run it");
        subject
            .run(&home, &["install", "gh:o/foo", "-t", "retail", "--json"])
            .ok();
        subject.run(&home, &["status", "--cached", "--json"]).ok();
        drop(subject);

        let records = read(&log, &dir);
        let context = format!("subject {}", dir.join("transcript.md").display());
        let runs = of_kind(&records, "run");
        let entries = read_net_log(&paths(&home.env));
        let purposes: Vec<&str> = entries.iter().map(|e| e.purpose.as_str()).collect();
        log.json("net.log purposes", &purposes);
        let added = runs[0]["net_log"]["added"].as_array().unwrap();
        assert_eq!(
            added.len(),
            entries.len(),
            "{context}: one recorded line per net.log line"
        );
        for (line, entry) in added.iter().zip(&entries) {
            let recorded = (
                &line["method"],
                &line["host"],
                &line["path"],
                &line["purpose"],
                &line["status"],
            );
            let expected = (
                &json!(entry.method),
                &json!(entry.host),
                &json!(entry.path),
                &json!(entry.purpose),
                &json!(entry.status),
            );
            assert_eq!(recorded, expected, "{context}: {line}");
        }
        assert!(
            purposes.contains(&"release-latest") && purposes.contains(&"release-json"),
            "{context}: {purposes:?}"
        );
        assert_eq!(runs[0]["net_log"]["rotated"], false, "{context}");
        assert_eq!(
            runs[0]["overrides"]["OMADDONS_HTTP_FIXTURES"], "<root>/http",
            "{context}"
        );
        assert_eq!(
            runs[1]["net_log"]["added"],
            json!([]),
            "{context}: status --cached asks nothing"
        );
    }

    #[test]
    fn failpoints_and_pauses_are_recorded() {
        let log = tlog!();
        log.step("build the subject");
        let scene = Scene::new();
        let pause = Pause::new(scene.home.env.root());
        let subject = TestLog::new(CHILD, "failpoints_and_pauses");
        let dir = subject.dir().to_path_buf();

        log.step("run it");
        let running = subject
            .cmd(&scene, &scene.install_args("gh:o/foo", "v1"))
            .failpoints("engine.plan.done=pause")
            .pause(&pause)
            .spawn();
        subject.reached(&pause, "engine.plan.done");
        subject.run(&scene, &["status", "--json"]).ok();
        subject.resume(&pause, "engine.plan.done");
        running.wait().ok();
        drop(subject);

        let records = read(&log, &dir);
        let context = format!("subject {}", dir.join("transcript.md").display());
        let pauses = of_kind(&records, "pause");
        let actions: Vec<&str> = pauses
            .iter()
            .map(|p| p["action"].as_str().unwrap())
            .collect();
        assert_eq!(actions, ["waiting", "reached", "resumed"], "{context}");
        assert!(pauses[1]["waited_ms"].is_u64(), "{context}");
        for pause in &pauses {
            let pause_dir = pause["dir"].as_str().unwrap();
            assert!(pause_dir.starts_with("<root>/"), "{context}: {pause}");
        }
        let runs = of_kind(&records, "run");
        let (status, install) = (runs[0], runs[1]);
        assert_eq!(
            status["argv"][1], "status",
            "{context}: the status run's line comes first"
        );
        assert!(
            install["seq"].as_u64() < status["seq"].as_u64(),
            "{context}: the install's seq was taken when it started"
        );
        assert_eq!(
            install["failpoints"]["spec"], "engine.plan.done=pause",
            "{context}"
        );
        let pause_dir = install["failpoints"]["pause_dir"].as_str().unwrap();
        assert!(pause_dir.starts_with("<root>/"), "{context}: {pause_dir}");
        assert_eq!(install["background"], true, "{context}");
        assert_eq!(status["background"], false, "{context}");
    }

    #[test]
    fn an_aborted_child_is_recorded_with_its_signal() {
        let log = tlog!();
        log.step("build the subject");
        let scene = Scene::new();
        let subject = TestLog::new(CHILD, "aborted_child");
        let dir = subject.dir().to_path_buf();

        log.step("run it");
        subject.step("install, crashing under the lock");
        let ran = subject
            .cmd(&scene, &scene.install_args("gh:o/foo", "v1"))
            .failpoints("engine.journal.prepared=abort")
            .run();
        ran.aborted();
        assert_eq!(ran.run.code, -1, "an abort reads as code -1");
        let caught = catch_unwind(AssertUnwindSafe(|| {
            ran.ok();
        }));
        let text = message(caught.expect_err("ok() fails on an aborted run"));
        log.note(format!("ok() said: {text}"));
        for expected in [
            "in step \"install, crashing under the lock\"",
            "expected exit 0, killed by signal 6",
            "transcript:",
        ] {
            assert!(
                text.contains(expected),
                "the panic lacks {expected:?}: {text}"
            );
        }
        drop(subject);

        let records = read(&log, &dir);
        let context = format!("subject {}", dir.join("transcript.md").display());
        let run = of_kind(&records, "run")[0];
        assert_eq!(
            (&run["exit"], &run["signal"]),
            (&json!(-1), &json!(6)),
            "{context}"
        );
    }

    #[test]
    fn a_hung_child_is_killed_at_its_timeout() {
        let log = tlog!();
        log.step("build the subject");
        let scene = Scene::new();
        let pause = Pause::new(scene.home.env.root());
        let subject = TestLog::new(CHILD, "hung_child");
        let dir = subject.dir().to_path_buf();

        log.step("run it");
        subject.step("an install paused for good");
        let running = subject
            .cmd(&scene, &scene.install_args("gh:o/foo", "v1"))
            .failpoints("engine.plan.done=pause")
            .pause(&pause)
            .timeout(Duration::from_millis(300))
            .spawn();
        let pid = running.pid();
        let caught = catch_unwind(AssertUnwindSafe(|| running.wait()));
        let text = message(caught.err().expect("a run past its timeout fails"));
        log.note(format!("wait() said: {text}"));
        assert!(
            text.contains("in step \"an install paused for good\"")
                && text.contains("timed out after 3"),
            "the panic names the step and the timeout: {text}"
        );
        let proc = PathBuf::from(format!("/proc/{pid}"));
        let deadline = Instant::now() + Duration::from_secs(1);
        while proc.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!proc.exists(), "the child {pid} is gone");
        drop(subject);

        let records = read(&log, &dir);
        let context = format!("subject {}", dir.join("transcript.md").display());
        let run = of_kind(&records, "run")[0];
        assert_eq!(run["timed_out"], true, "{context}");
        assert!(run["elapsed_ms"].as_u64().unwrap() >= 300, "{context}");
    }

    #[test]
    // ubs:ignore — the planted values below are fakes; the test asserts they never reach the log
    fn a_planted_token_never_reaches_the_files() {
        // ubs:ignore — a fake planted value; the test asserts it never reaches the log
        const TOKEN: &str = "tlogSelftestToken-0123456789abcdef";
        // ubs:ignore — a fake planted value; the test asserts it never reaches the log
        const OTHER: &str = "tlogSelftestOther-fedcba9876543210";
        let log = tlog!();
        log.step("build the subject");
        let env = TestEnv::new();
        let subject = TestLog::new(CHILD, "planted_token");
        let dir = subject.dir().to_path_buf();

        log.step("run it");
        subject
            .cmd(&env, &["status", "--json"])
            .env("OMADDONS_GITHUB_TOKEN", TOKEN)
            .run()
            .ok();
        subject.secret(OTHER);
        subject.note(format!("planted {OTHER} twice: {OTHER}"));
        subject.json("blob", &json!({"value": OTHER}));
        drop(subject);

        let records = read(&log, &dir);
        let context = format!("subject {}", dir.join("transcript.md").display());
        for name in ["steps.jsonl", "transcript.md"] {
            let text = file(&dir, name);
            assert!(
                !text.contains(TOKEN) && !text.contains(OTHER),
                "{context}: {name} holds a planted value"
            );
        }
        let run = of_kind(&records, "run")[0];
        assert_eq!(
            run["overrides"]["OMADDONS_GITHUB_TOKEN"], "<redacted>",
            "{context}"
        );
        assert_eq!(
            run["scrubbed"],
            json!(["OMADDONS_GITHUB_TOKEN"]),
            "{context}"
        );
        assert_eq!(
            of_kind(&records, "note")[0]["text"],
            "planted <redacted> twice: <redacted>",
            "{context}"
        );
        assert_eq!(
            of_kind(&records, "json")[0]["value"],
            json!({"value": "<redacted>"}),
            "{context}"
        );
    }

    #[test]
    fn truncation_and_capping_are_recorded() {
        let log = tlog!();
        log.step("build the subject");
        let env = TestEnv::new();
        let caps = Caps {
            stream_bytes: 256,
            test_bytes: 8 << 10,
        };
        let subject = TestLog::with_caps(CHILD, "truncation", caps);
        let dir = subject.dir().to_path_buf();

        log.step("run it until the cap");
        let first = subject.run(&env, &["help", "--json"]);
        let stdout_len = first.stdout().len() as u64;
        let mut runs: u64 = 1;
        let capped = || file(&dir, "steps.jsonl").contains("\"kind\":\"capped\"");
        while !capped() {
            assert!(runs < 100, "the log never reached its cap");
            subject.run(&env, &["help", "--json"]);
            runs += 1;
        }
        let after_cap: u64 = 3;
        for _ in 0..after_cap {
            subject.run(&env, &["help", "--json"]);
        }
        drop(subject);

        let records = read(&log, &dir);
        let context = format!("subject {}", dir.join("transcript.md").display());
        let first = of_kind(&records, "run")[0];
        assert_eq!(
            first["stdout"]["form"], "text",
            "{context}: a cut envelope is text"
        );
        let kept = first["stdout"]["truncated"]["kept"].as_u64().unwrap();
        assert!((253..=256).contains(&kept), "{context}: kept {kept}");
        assert_eq!(
            first["stdout"]["truncated"]["total"], stdout_len,
            "{context}"
        );
        let kinds = kinds(&records);
        // ubs:ignore — comparing record kinds, not secrets
        let capped_at = kinds.iter().position(|k| *k == "capped");
        let capped_count = kinds.iter().filter(|k| **k == "capped").count(); // ubs:ignore — record kinds
        assert_eq!(capped_count, 1, "{context}");
        assert!(
            !kinds[capped_at.unwrap()..].contains(&"run"),
            "{context}: no run after the cap"
        );
        let written = of_kind(&records, "run").len() as u64;
        let end = records.last().unwrap();
        assert_eq!(
            end["dropped"],
            runs + after_cap - written,
            "{context}: the runs left out"
        );
        let end_line = file(&dir, "steps.jsonl").lines().last().unwrap().len() as u64 + 1;
        let transcript = file(&dir, "transcript.md");
        let end_part = (transcript.len() - transcript.rfind("\n---\n\nend: ").unwrap()) as u64;
        let before_end = bytes(&dir) - end_line - end_part;
        assert!(
            before_end <= 8 << 10,
            "{context}: {before_end} bytes before end"
        );
        assert!(
            transcript.contains("truncated: kept") && transcript.contains("log capped"),
            "{context}: the transcript says what was cut and capped"
        );
    }

    #[test]
    fn tree_differences_name_the_paths() {
        let log = tlog!();
        log.step("build the subject");
        let env = TestEnv::new();
        let tree = env.root().join("tree");
        fs::create_dir_all(&tree).unwrap();
        fs::write(tree.join("a.txt"), "alpha").unwrap();
        fs::write(tree.join("b.sh"), "#!/bin/sh\n").unwrap();
        fs::set_permissions(tree.join("b.sh"), fs::Permissions::from_mode(0o644)).unwrap();
        symlink("a.txt", tree.join("link")).unwrap();
        let subject = TestLog::new(CHILD, "tree_differences");
        let dir = subject.dir().to_path_buf();
        subject.env(&env);
        subject.tree("t", &tree);

        log.step("change it");
        fs::write(tree.join("a.txt"), "alphA").unwrap();
        fs::set_permissions(tree.join("b.sh"), fs::Permissions::from_mode(0o755)).unwrap();
        fs::remove_file(tree.join("link")).unwrap();
        symlink("b.sh", tree.join("link")).unwrap();
        let expected = ["~ a.txt (content)", "~ b.sh (mode)", "~ link (target)"];
        let caught = catch_unwind(AssertUnwindSafe(|| subject.assert_unchanged("t", &tree)));
        let text = message(caught.expect_err("a changed tree fails"));
        log.note(format!("assert_unchanged said: {text}"));
        for line in expected {
            assert!(text.contains(line), "the panic lacks {line:?}: {text}");
        }
        let diff = subject.diff("t", &tree);
        let wanted: Vec<String> = expected.iter().map(|line| (*line).to_owned()).collect();
        log.assert_eq("diff", &diff.first(20), &wanted);
        subject.tree("t", &tree);
        subject.assert_unchanged("t", &tree);
        let gone = subject.tree("gone", &env.root().join("missing"));
        assert_eq!(gone, None, "an absent root has no hash");
        drop(subject);

        let records = read(&log, &dir);
        let context = format!("subject {}", dir.join("transcript.md").display());
        let checks = of_kind(&records, "tree_check");
        assert_eq!(checks[0]["equal"], false, "{context}");
        assert_eq!(checks[0]["diff"]["total"], 3, "{context}");
        assert_eq!(checks[0]["diff"]["first"], json!(expected), "{context}");
        assert_eq!(checks.last().unwrap()["equal"], true, "{context}");
        let trees = of_kind(&records, "tree");
        let gone = trees.last().unwrap();
        assert_eq!(
            (&gone["exists"], &gone["hash"]),
            (&json!(false), &Value::Null),
            "{context}"
        );
    }

    #[test]
    fn tree_roots_outside_the_test_are_refused() {
        let log = tlog!();
        let subject = TestLog::new(CHILD, "refused_roots");
        let dir = subject.dir().to_path_buf();
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
        let caught = catch_unwind(AssertUnwindSafe(|| subject.tree("repo", repo)));
        let text = message(caught.expect_err("the repository is refused"));
        log.note(format!("tree said: {text}"));
        assert!(
            text.contains("outside this test's synthetic directories"),
            "{text}"
        );
        drop(subject);
        let records = read(&log, &dir);
        assert!(
            of_kind(&records, "tree").is_empty(),
            "no tree was recorded for the repository"
        );
    }

    #[test]
    fn ran_assertions_name_the_step_and_the_transcript() {
        let log = tlog!();
        let env = TestEnv::new();
        let subject = TestLog::new(CHILD, "ran_assertions");
        let dir = subject.dir().to_path_buf();
        subject.step("a usage error");
        let ran = subject.run(&env, &["frobnicate", "--json"]);
        let caught = catch_unwind(AssertUnwindSafe(|| {
            ran.exit(0);
        }));
        let text = message(caught.expect_err("exit(0) fails on exit 2"));
        log.note(format!("exit(0) said: {text}"));
        for expected in [
            "in step \"a usage error\"",
            "expected exit 0, got 2",
            "error: usage: ",
            "argv: omaddons frobnicate --json",
            "transcript: ",
        ] {
            assert!(
                text.contains(expected),
                "the panic lacks {expected:?}: {text}"
            );
        }
        drop(subject);
        let records = read(&log, &dir);
        let kinds = kinds(&records);
        // ubs:ignore — comparing record kinds, not secrets
        let run = kinds.iter().position(|k| *k == "run").unwrap();
        assert_eq!(
            kinds[run + 1],
            "failure",
            "a failure record follows the run: {kinds:?}"
        );
    }

    #[test]
    fn cases_have_their_own_files_and_share_the_budget() {
        let log = tlog!();
        log.step("build the subject");
        let env = TestEnv::new();
        let caps = Caps {
            stream_bytes: 1 << 20,
            test_bytes: 64 << 10,
        };
        let subject = TestLog::with_caps(CHILD, "cases", caps);
        let dir = subject.dir().to_path_buf();

        log.step("run two cases from two threads");
        std::thread::scope(|scope| {
            for i in 0..2 {
                let (subject, env) = (&subject, &env);
                scope.spawn(move || {
                    let case = subject.case(&format!("case-{i}"));
                    case.step("run");
                    case.run(env, &["help", "--json"]).ok();
                });
            }
        });
        drop(subject);

        let parent = read(&log, &dir);
        let context = format!("subject {}", dir.join("transcript.md").display());
        assert_eq!(of_kind(&parent, "case").len(), 2, "{context}");
        let mut total = parent.last().unwrap()["bytes"].as_u64().unwrap();
        for i in 0..2 {
            let case = records(&dir.join(format!("cases/case-{i}")));
            assert_eq!(case[0]["parent"], "tlog_selftest_child/cases", "case {i}");
            assert_eq!(of_kind(&case, "run").len(), 1, "case {i}");
            total += case.last().unwrap()["bytes"].as_u64().unwrap();
        }
        assert!(
            total <= 64 << 10,
            "{context}: {total} bytes in the parent and its cases"
        );

        log.step("a 4 KiB budget caps the second case too");
        let small = Caps {
            stream_bytes: 1 << 20,
            test_bytes: 4 << 10,
        };
        let subject = TestLog::with_caps(CHILD, "cases_capped", small);
        let dir = subject.dir().to_path_buf();
        for i in 0..2 {
            let case = subject.case(&format!("case-{i}"));
            case.run(&env, &["help", "--json"]);
        }
        drop(subject);
        for i in 0..2 {
            let case = records(&dir.join(format!("cases/case-{i}")));
            assert_eq!(
                of_kind(&case, "capped").len(),
                1,
                "case {i} of cases_capped is capped: {:?}",
                kinds(&case)
            );
        }
    }

    #[test]
    fn paths_are_relative_in_both_files() {
        let log = tlog!();
        let env = TestEnv::new();
        let subject = TestLog::new(CHILD, "relative_paths");
        let dir = subject.dir().to_path_buf();
        subject.run(&env, &["help", "--json"]).ok();
        subject
            .run(&env, &["frobnicate", "--json"])
            .fails(2, "usage");
        subject.run(&env, &["status", "--json"]).ok();
        subject.tree("state", &env.state());
        drop(subject);
        let _ = read(&log, &dir);
        let absolute = [
            env.root().display().to_string(),
            env!("CARGO_TARGET_TMPDIR").to_owned(),
            env!("CARGO_MANIFEST_DIR").to_owned(),
        ];
        for name in ["steps.jsonl", "transcript.md"] {
            let text = file(&dir, name);
            for path in &absolute {
                assert!(
                    !text.contains(path.as_str()),
                    "{name} holds the absolute path {path}"
                );
            }
            assert!(
                text.contains("<root>/") && text.contains("<repo>"),
                "{name} uses the placeholders"
            );
        }
    }
}

/// What only a separate process shows: libtest's failure output with the guard's lines,
/// the live echo, and the mode error. Each parent runs one child test of this binary
/// (`--ignored --exact child::<name>`) with `OMADDONS_TEST_LOG` set or removed
/// explicitly, never inherited (`argc e2e` sets it for the parent).
mod child {
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::thread::JoinHandle;
    use std::time::{Duration, Instant};

    use serde_json::{Value, json};

    use crate::common::env::TestEnv;
    use crate::common::tlog::{LOG_MODE_VAR, TestLog, log_root, tlog};

    /// The private handshake that lets a child test do its work: not an `OMADDONS_`
    /// name, so it needs no registry row and `TestEnv` leaves it alone.
    const HANDSHAKE: &str = "TLOG_SELFTEST_CHILD";
    /// The longest a child may take.
    const CHILD_TIMEOUT: Duration = Duration::from_secs(60);
    /// The suite directory of the children's logs.
    const CHILD: &str = "tlog_selftest_child";

    fn is_child() -> bool {
        // ubs:ignore — a test handshake flag, not a secret
        std::env::var(HANDSHAKE).as_deref() == Ok("1")
    }

    /// A child's exit code and output.
    struct Output {
        code: Option<i32>,
        stdout: String,
        stderr: String,
    }

    fn read_to_end<R: Read + Send + 'static>(mut pipe: R) -> JoinHandle<String> {
        std::thread::spawn(move || {
            let mut text = String::new();
            let _ = pipe.read_to_string(&mut text);
            text
        })
    }

    /// Runs the child test `child::<name>` with `mode` as `OMADDONS_TEST_LOG` (`None`
    /// removes it), and logs its argv, exit and output.
    fn run_child(log: &TestLog, name: &str, mode: Option<&str>) -> Output {
        let test = format!("child::{name}");
        let args = ["--ignored", "--exact", test.as_str(), "--test-threads=1"];
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(args)
            .env(HANDSHAKE, "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        match mode {
            Some(mode) => command.env(LOG_MODE_VAR, mode),
            None => command.env_remove(LOG_MODE_VAR),
        };
        let mut child = command.spawn().unwrap();
        let stdout = read_to_end(child.stdout.take().unwrap());
        let stderr = read_to_end(child.stderr.take().unwrap());
        let deadline = Instant::now() + CHILD_TIMEOUT;
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break Some(status);
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let output = Output {
            code: status.and_then(|status| status.code()),
            stdout: stdout.join().unwrap(),
            stderr: stderr.join().unwrap(),
        };
        log.json(
            "child",
            &json!({
                "argv": args,
                "mode": mode,
                "exit": output.code,
                "stdout": output.stdout,
                "stderr": output.stderr,
            }),
        );
        assert!(
            status.is_some(),
            "child {name} ran past {CHILD_TIMEOUT:?} and was killed"
        );
        output
    }

    /// `text` holds each of `parts`, in this order.
    fn in_order(text: &str, parts: &[&str]) -> Result<(), String> {
        let mut from = 0;
        for part in parts {
            match text[from..].find(part) {
                Some(at) => from += at + part.len(),
                None => return Err(format!("{part:?} is missing, or out of order")),
            }
        }
        Ok(())
    }

    #[test]
    #[ignore = "run by a parent test as a child process"]
    fn fails_after_twenty_five_notes() {
        if !is_child() {
            return;
        }
        let log = TestLog::new(CHILD, "fails_after_twenty_five_notes");
        for i in 1..=25 {
            log.note(format!("note {i:02}"));
        }
        log.run(&TestEnv::new(), &["frobnicate", "--json"]).ok();
    }

    #[test]
    #[ignore = "run by a parent test as a child process"]
    fn passes_with_three_steps() {
        if !is_child() {
            return;
        }
        let log = TestLog::new(CHILD, "passes_with_three_steps");
        log.step("one");
        log.step("two");
        log.step("three");
        log.run(&TestEnv::new(), &["help", "--json"]).ok();
    }

    #[test]
    #[ignore = "run by a parent test as a child process"]
    fn creates_one_log() {
        if !is_child() {
            return;
        }
        let log = TestLog::new(CHILD, "creates_one_log");
        log.note("not reached with an unknown mode");
    }

    #[test]
    fn a_failing_test_prints_its_log_and_last_steps() {
        let log = tlog!();
        log.step("run the failing child");
        let output = run_child(&log, "fails_after_twenty_five_notes", None);
        log.step("check its output");
        assert_ne!(output.code, Some(0), "the child fails");
        let text = format!("{}{}", output.stdout, output.stderr);
        let order = in_order(
            &text,
            &[
                "expected exit 0, got 2",
                "test log: ",
                "tlog_selftest_child/fails_after_twenty_five_notes/transcript.md",
                "last 20 of 29 records:",
                "#10 note: note 09",
                "#26 note: note 25",
                "#28 run omaddons frobnicate --json → exit 2",
                "#29 failure: run #28",
            ],
        );
        assert!(order.is_ok(), "{}: {text}", order.unwrap_err());
        assert!(
            !text.contains("note 08"),
            "only the last 20 records are shown: {text}"
        );

        let dir = log_root().join(CHILD).join("fails_after_twenty_five_notes");
        let steps = std::fs::read_to_string(dir.join("steps.jsonl")).unwrap();
        let tail: Vec<Value> = steps
            .lines()
            .rev()
            .take(2)
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        log.json("the child's last records", &tail);
        assert_eq!(
            (&tail[1]["kind"], &tail[0]["kind"], &tail[0]["outcome"]),
            (&json!("failure"), &json!("end"), &json!("panicked")),
            "the child's log ends with its failure, then a panicked end"
        );
    }

    #[test]
    fn verbose_echoes_every_record_live() {
        let log = tlog!();
        log.step("verbose");
        let output = run_child(&log, "passes_with_three_steps", Some("verbose"));
        assert_eq!(output.code, Some(0), "the child passes");
        let prefix = "[tlog tlog_selftest_child::passes_with_three_steps";
        let lines = [
            format!("{prefix} #2] step 1: one"),
            format!("{prefix} #3] step 2: two"),
            format!("{prefix} #4] step 3: three"),
            format!("{prefix} #6] run omaddons help --json → exit 0"),
        ];
        let parts: Vec<&str> = lines.iter().map(String::as_str).collect();
        let order = in_order(&output.stderr, &parts);
        assert!(order.is_ok(), "{}: {}", order.unwrap_err(), output.stderr);

        log.step("unset");
        let output = run_child(&log, "passes_with_three_steps", None);
        assert_eq!(output.code, Some(0), "the child passes");
        assert!(
            !output.stderr.contains("[tlog "),
            "no echo without the mode: {}",
            output.stderr
        );
    }

    #[test]
    fn an_unknown_log_mode_fails_the_test() {
        let log = tlog!();
        let output = run_child(&log, "creates_one_log", Some("verbos"));
        assert_ne!(output.code, Some(0), "the child fails");
        let text = format!("{}{}", output.stdout, output.stderr);
        assert!(
            text.contains("OMADDONS_TEST_LOG=verbos is not a log mode")
                && text.contains("`verbose`"),
            "the failure names the accepted value: {text}"
        );
    }
}
