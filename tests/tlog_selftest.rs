//! The structured test log (`tests/common/tlog.rs`) keeps its promises: its pure
//! helpers (secret names and values, path placeholders, caps, the test budget, names,
//! modes) case by case, and the two files a log writes, read back after it is dropped.
//! Subject logs are written under the suite directory `tlog_selftest_child`, which is
//! never a real suite, so `argc e2e` never summarizes their deliberate failures.

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
