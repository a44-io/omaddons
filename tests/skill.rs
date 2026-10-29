//! e2e suite: skill
//!
//! `skills/omaddons/SKILL.md` is the guide `omaddons robot-docs guide` renders, byte for
//! byte (M4-D7). `OMADDONS_UPDATE_SKILL=1 cargo test --test skill` rewrites it from a
//! hermetic home, never the developer's. Only `skill_md_is_the_guide` reads or writes the
//! file, so a regeneration never races a reader.

mod common;

use common::env::TestEnv;
use common::guide::{self, REGENERATE, SKILL_FILE};
use common::tlog::tlog;
use serde_json::json;

/// Lines of each side logged on a mismatch.
const DIFF_LINES: usize = 20;

#[test]
fn skill_md_is_the_guide() {
    let log = tlog!();
    let env = TestEnv::new();
    log.step("render");
    let human = log.run(&env, &["robot-docs", "guide"]);
    human.ok();
    let json = log.run(&env, &["robot-docs", "guide", "--json"]);
    json.ok();
    for ran in [&human, &json] {
        log.assert_eq("stderr", &ran.stderr(), &"");
    }
    log.assert_eq(
        "command",
        &json.envelope()["command"],
        &json!("robot-docs guide"),
    );
    log.assert_eq("format", &json.data()["format"], &json!("markdown"));
    let rendered = human.stdout().to_owned();
    log.assert_eq(
        "the JSON content is the human output",
        &(json.data()["content"].as_str() == Some(rendered.as_str())),
        &true,
    );

    log.step("compare");
    let committed = guide::committed();
    log.json(
        "texts",
        &json!({"committed": guide::summary(committed.as_deref()), "rendered": guide::summary(Some(&rendered))}),
    );
    if committed.as_deref() == Some(rendered.as_str()) {
        digest_matches(&log, &rendered);
        return;
    }
    if std::env::var("OMADDONS_UPDATE_SKILL").as_deref() == Ok("1") {
        let path = guide::skill_path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::write(&path, &rendered).unwrap();
        log.note(format!(
            "wrote {SKILL_FILE}; run cargo test --test skill again to check the compiled digest"
        ));
        log.step("digest");
        log.note("digest check deferred: this test binary was compiled before the file changed; run cargo test --test skill again");
        return;
    }
    let size = |text: &str| format!("{} lines / {} bytes", text.lines().count(), text.len());
    let found = match &committed {
        None => "missing".to_owned(),
        Some(text) => {
            let difference = guide::first_difference(text, &rendered);
            if let Some(d) = &difference {
                log.json(
                    "differing lines",
                    &json!({"from line": d.line, "committed": window(text, d.line), "rendered": window(&rendered, d.line)}),
                );
            }
            match difference {
                Some(d) => format!(
                    "first difference at line {} (committed: `{}`, rendered: `{}`); committed {}",
                    d.line,
                    d.committed,
                    d.rendered,
                    size(text)
                ),
                None => format!("committed {}", size(text)),
            }
        }
    };
    let message = format!(
        "{SKILL_FILE} is not the guide this build renders: {found}, rendered {}. Regenerate it with `{REGENERATE}` and commit it.",
        size(&rendered)
    );
    log.note(&message);
    let failure = format!(
        "{message}\n  step: compare\n  transcript: {}",
        log.transcript_path().display()
    );
    panic!("{failure}"); // ubs:ignore — the test's own failure, after logging it
}

/// The digest `build.rs` compiled in from the committed file is the rendered guide's
/// stamp: the binary knows its guide without rendering it.
fn digest_matches(log: &common::tlog::TestLog, rendered: &str) {
    log.step("digest");
    let stamp = guide::layout(rendered).map(|layout| layout.stamp_hex);
    let expected = omaddons::contract::guide::EXPECTED_DIGEST;
    log.json(
        "digests",
        &json!({"rendered_stamp": stamp, "expected_digest": expected}),
    );
    let matches = matches!(stamp.as_deref(), Ok(recorded) if recorded.eq(expected));
    if !matches {
        let failure = format!(
            "the compiled guide digest (EXPECTED_DIGEST {expected}) is not the rendered guide's stamp ({stamp:?}): rebuild after regenerating, or regenerate with `{REGENERATE}`\n  step: digest\n  transcript: {}",
            log.transcript_path().display()
        );
        panic!("{failure}"); // ubs:ignore — the test's own failure, after logging both digests
    }
}

/// Up to [`DIFF_LINES`] lines of `text` from 1-based `line`.
fn window(text: &str, line: usize) -> Vec<&str> {
    text.lines().skip(line - 1).take(DIFF_LINES).collect()
}

#[test]
fn skill_md_opens_with_frontmatter_then_stamp() {
    let log = tlog!();
    let env = TestEnv::new();
    log.step("render");
    let text = guide::rendered(&log, &env);
    log.step("layout");
    let layout = guide::layout(&text);
    log.assert_eq("layout errors", &layout.as_ref().err(), &None);
    if let Ok(layout) = layout {
        log.json("layout", &layout);
        log.assert_eq("name", &layout.name.as_str(), &"omaddons");
    }
}

/// Regenerating, rebuilding and regenerating again leaves the bytes and the digest
/// unchanged: the guide depends only on the template and the registries, the build id
/// never covers `skills/`, and no block reads the compiled digest.
#[test]
fn regeneration_is_a_fixed_point() {
    use omaddons::build_info::inputs::{BUILD_ID_EXCLUDED, BUILD_ID_INPUTS};
    use omaddons::contract::guide::render::{GuideInputs, Provenance, render};
    use omaddons::contract::guide::{EXPECTED_DIGEST, TEMPLATE, render_guide};

    let log = tlog!();
    let env = TestEnv::new();
    let with = |version: &str, build_id: &str| {
        let mut inputs = GuideInputs::live();
        inputs.provenance = Provenance {
            tool_version: version.to_owned(),
            build_id: Some(build_id.to_owned()),
        };
        render(TEMPLATE, &inputs).unwrap()
    };

    log.step("renderer ignores provenance");
    let (a, b) = (
        with("0.1.0", "0000000000000000"),
        with("9.9.9", "ffffffffffffffff"),
    );
    log.json(
        "renders",
        &json!({"a": guide::summary(Some(&a)), "b": guide::summary(Some(&b))}),
    );
    log.assert_eq("the two renders are equal", &(a == b), &true);

    log.step("the binary prints the library's guide");
    let printed = guide::rendered(&log, &env);
    log.assert_eq(
        "the binary's guide is render_guide()'s",
        &(Ok(printed.clone()) == render_guide()),
        &true,
    );

    log.step("the build id excludes skills/");
    let inputs: Vec<&str> = BUILD_ID_INPUTS.iter().map(|input| input.path()).collect();
    let excluded: Vec<&str> = BUILD_ID_EXCLUDED.iter().map(|(name, _)| *name).collect();
    log.json("build id", &json!({"inputs": inputs, "excluded": excluded}));
    let feeding: Vec<&str> = inputs
        .iter()
        .copied()
        .filter(|path| *path == "skills" || path.starts_with("skills/"))
        .collect();
    log.assert_eq("inputs under skills/", &feeding, &Vec::new());
    log.assert_eq("skills is excluded", &excluded.contains(&"skills"), &true);

    log.step("no build identity in the guide");
    let build_id = omaddons::build_info::BUILD_ID;
    log.assert_eq(
        "the guide holds the build id",
        &printed.contains(build_id),
        &false,
    );

    log.step("the stamp is the compiled digest");
    let stamps: Vec<Option<String>> = [&printed, &b]
        .iter()
        .map(|text| guide::layout(text).ok().map(|layout| layout.stamp_hex))
        .collect();
    log.json("stamps", &json!({"printed": stamps[0], "other_provenance": stamps[1], "expected_digest": EXPECTED_DIGEST}));
    let want = Some(EXPECTED_DIGEST.to_owned());
    log.assert_eq("stamps", &stamps, &vec![want.clone(), want]);
}

#[test]
fn every_command_the_guide_names_parses() {
    let log = tlog!();
    let env = TestEnv::new();
    log.step("extract");
    let text = guide::rendered(&log, &env);
    let commands = guide::commands(&text);
    log.json("commands", &commands);
    log.assert_eq("the guide names commands", &commands.is_empty(), &false);
    log.step("fill and parse");
    let unparsable = guide::unparsable(&text);
    log.assert_eq("commands that do not parse", &unparsable, &Vec::new());
}

#[test]
fn a_planted_unparsable_span_is_reported() {
    let log = tlog!();
    log.step("plant");
    let text = "\
---
name: x
---
<!-- omaddons guide sha256:0000000000000000000000000000000000000000000000000000000000000000 -->
Run `omaddons status --json`, `omaddons nosuch`, `omaddons status <nope>` and `omaddons install …`.

```
omaddons help install
omaddons frobnicate
```
";
    let reported = guide::unparsable(text);
    log.json("reported", &reported);
    let spans: Vec<&str> = reported.iter().map(|(span, _)| span.as_str()).collect();
    log.assert_eq(
        "the spans reported",
        &spans,
        &vec![
            "omaddons nosuch",
            "omaddons status <nope>",
            "omaddons install …",
            "omaddons frobnicate",
        ],
    );
    let reason = |span: &str| {
        reported
            .iter()
            .find(|(s, _)| s == span)
            .map(|(_, why)| why.clone())
            .unwrap_or_default()
    };
    log.assert_eq(
        "<nope>",
        &reason("omaddons status <nope>"),
        &"add a sample for <nope> in SAMPLES".to_owned(),
    );
    log.assert_eq(
        "…",
        &reason("omaddons install …"),
        &"write the command out in full".to_owned(),
    );
    log.assert_eq(
        "an unknown command names clap's refusal",
        &reason("omaddons nosuch").starts_with("clap refuses `omaddons nosuch`: "),
        &true,
    );
}

/// The guide's size beside the budget B2 enforces. A report: it never fails.
#[test]
fn guide_size_is_reported() {
    const LINES_BUDGET: usize = 500;
    const BYTES_BUDGET: usize = 32 << 10;
    let log = tlog!();
    let env = TestEnv::new();
    log.step("size");
    let text = guide::rendered(&log, &env);
    log.json(
        "size",
        &json!({
            "lines": text.lines().count(),
            "lines_budget": LINES_BUDGET,
            "bytes": text.len(),
            "bytes_budget": BYTES_BUDGET,
        }),
    );
}
