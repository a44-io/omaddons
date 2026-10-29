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
