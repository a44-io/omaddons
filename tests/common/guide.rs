//! The agent guide in tests: where the committed skill lives, the rendered guide, and an
//! independent check of its frontmatter and stamp. Every guide test calls these instead of
//! copying them.

use std::path::PathBuf;

use clap::Parser as _;
use serde::Serialize;
use serde_json::json;
use sha2::{Digest, Sha256};

use super::tlog::{Runner, TestLog};

/// The committed skill, relative to the repository.
pub const SKILL_FILE: &str = "skills/omaddons/SKILL.md";
/// The command that rewrites it from the rendered guide.
pub const REGENERATE: &str = "OMADDONS_UPDATE_SKILL=1 cargo test --test skill";

const STAMP_PREFIX: &str = "<!-- omaddons guide sha256:";
const STAMP_SUFFIX: &str = " -->";

pub fn skill_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(SKILL_FILE)
}

/// The committed skill; `None` when the file does not exist.
pub fn committed() -> Option<String> {
    std::fs::read_to_string(skill_path()).ok()
}

/// `robot-docs guide`'s stdout on `on`: exit 0, nothing on stderr. Logs its digest and
/// size.
pub fn rendered(log: &TestLog, on: &impl Runner) -> String {
    let ran = log.run(on, &["robot-docs", "guide"]);
    ran.ok();
    log.assert_eq("stderr", &ran.stderr(), &"");
    let text = ran.stdout().to_owned();
    log.json("rendered", &summary(Some(&text)));
    text
}

/// `{present, sha256, lines, bytes}` of a text that may be missing.
pub fn summary(text: Option<&str>) -> serde_json::Value {
    match text {
        Some(text) => json!({
            "present": true,
            "sha256": sha256_hex(text),
            "lines": text.lines().count(),
            "bytes": text.len(),
        }),
        None => json!({"present": false}),
    }
}

/// What the skill's first five lines say.
#[derive(Debug, Serialize)]
pub struct Layout {
    pub name: String,
    pub description: String,
    pub stamp_hex: String,
    pub lines: usize,
    pub bytes: usize,
}

/// The frontmatter and stamp, checked exactly: `---`, `name: <value>`, `description:
/// <value>`, `---`, then the stamp, whose hex must be the SHA-256 of the text without
/// that line. Computed here with `sha2`, independent of `contract::guide::stamp`, so the
/// library's algorithm is cross-checked rather than trusted. Each failure is a sentence
/// naming the line.
pub fn layout(text: &str) -> Result<Layout, String> {
    let lines: Vec<&str> = text.split('\n').collect();
    let line = |n: usize| lines.get(n - 1).copied().unwrap_or("");
    if line(1) != "---" {
        return Err(format!("line 1 must be `---`, found {:?}", line(1)));
    }
    let name = scalar(line(2), "name", 2)?;
    let description = scalar(line(3), "description", 3)?;
    if line(4) != "---" {
        return Err(format!(
            "line 4 must be `---` (the frontmatter holds name and description only), found {:?}",
            line(4)
        ));
    }
    let stamp = line(5);
    let hex = stamp
        .strip_prefix(STAMP_PREFIX)
        .and_then(|rest| rest.strip_suffix(STAMP_SUFFIX))
        .filter(|hex| {
            hex.len() == 64
                && hex
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
        .ok_or_else(|| {
            format!(
                "line 5 must be `{STAMP_PREFIX}<64 lowercase hex>{STAMP_SUFFIX}`, found {stamp:?}"
            )
        })?;
    if lines.len() < 6 {
        return Err("line 5, the stamp, must end with a newline".to_owned());
    }
    let start: usize = lines[..4].iter().map(|l| l.len() + 1).sum();
    let end = start + stamp.len() + 1;
    let without = format!("{}{}", &text[..start], &text[end..]);
    let computed = sha256_hex(&without);
    if !computed.eq(hex) {
        return Err(format!(
            "line 5's stamp records {hex}, but the text without that line hashes to {computed}"
        ));
    }
    Ok(Layout {
        name,
        description,
        stamp_hex: hex.to_owned(),
        lines: text.lines().count(),
        bytes: text.len(),
    })
}

/// The value of `<key>: <value>`, a YAML plain scalar safe without a parser: non-empty,
/// no indicator first, no `: ` or ` #` inside, no trailing space.
fn scalar(line: &str, key: &str, number: usize) -> Result<String, String> {
    let value = line
        .strip_prefix(key)
        .and_then(|rest| rest.strip_prefix(": "))
        .ok_or_else(|| format!("line {number} must be `{key}: <value>`, found {line:?}"))?;
    let indicator = value.starts_with(|c: char| "-?:,[]{}#&*!|>'\"%@`".contains(c));
    let safe = !value.is_empty()
        && !indicator
        && !value.contains(": ")
        && !value.contains(" #")
        && !value.ends_with(' ');
    if safe {
        Ok(value.to_owned())
    } else {
        Err(format!(
            "line {number}: {key}'s value is not a safe YAML plain scalar (non-empty, no indicator first, no `: ` or ` #`, no trailing space): {value:?}"
        ))
    }
}

/// Where two texts first differ, by 1-based line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Difference {
    pub line: usize,
    pub committed: String,
    pub rendered: String,
}

pub fn first_difference(committed: &str, rendered: &str) -> Option<Difference> {
    let (a, b): (Vec<&str>, Vec<&str>) = (
        committed.split('\n').collect(),
        rendered.split('\n').collect(),
    );
    (0..a.len().max(b.len()))
        .find(|&i| a.get(i) != b.get(i))
        .map(|i| Difference {
            line: i + 1,
            committed: a.get(i).copied().unwrap_or("<end of file>").to_owned(),
            rendered: b.get(i).copied().unwrap_or("<end of file>").to_owned(),
        })
}

/// A sample for each `<name>` a command in the guide may hold, so the command can be
/// parsed as an agent would type it. B2 extends it for its recipes.
pub const SAMPLES: &[(&str, &str)] = &[
    ("<command>", "install"),
    ("<target>", "retail"),
    ("<key>", ULID),
    ("<id>", ULID),
    ("<plan_id>", ULID),
    ("<ref>", "dbm"),
    // The fix patterns of the common errors.
    ("<name>", "dbm"),
    ("<folder>", "DBM-Core"),
    ("<tx>", ULID),
    ("<path>", "/games/World of Warcraft"),
];

const ULID: &str = "01J00000000000000000000000";

/// Every command the guide names that does not parse, with why. Each `<name>` takes its
/// [`SAMPLES`] value; an elision (`…`) or an unknown `<name>` is reported, and so is
/// anything clap refuses (its help and version count as parsing).
pub fn unparsable(text: &str) -> Vec<(String, String)> {
    commands(text)
        .into_iter()
        .filter_map(|span| reason(&span).map(|why| (span, why)))
        .collect()
}

/// The commands the guide names: each inline code span, and each line of a fenced block,
/// that starts with `omaddons `, in the body after the stamp line (the frontmatter's
/// description is a loading trigger, not a command an agent runs).
pub fn commands(text: &str) -> Vec<String> {
    let body = text
        .split_once(STAMP_PREFIX)
        .and_then(|(_, rest)| rest.split_once('\n'))
        .map_or(text, |(_, body)| body);
    let mut spans = Vec::new();
    let mut fenced = false;
    for line in body.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            spans.push(line.trim().to_owned());
        } else {
            spans.extend(line.split('`').skip(1).step_by(2).map(str::to_owned));
        }
    }
    spans.retain(|span| span.starts_with("omaddons "));
    spans
}

/// Why `span` would not run as typed, if it would not.
fn reason(span: &str) -> Option<String> {
    if span.contains('…') {
        return Some("write the command out in full".to_owned());
    }
    let mut filled = span.to_owned();
    while let Some(open) = filled.find('<') {
        let close = filled[open..].find('>').map(|i| open + i + 1)?;
        let name = filled[open..close].to_owned();
        let Some((_, sample)) = SAMPLES.iter().find(|(n, _)| *n == name) else {
            return Some(format!("add a sample for {name} in SAMPLES"));
        };
        filled.replace_range(open..close, &shell_words::quote(sample));
    }
    let words = match shell_words::split(&filled) {
        Ok(words) => words,
        Err(e) => return Some(format!("does not split as shell words: {e}")),
    };
    match omaddons::cli::Cli::try_parse_from(&words) {
        Ok(_) => None,
        Err(e)
            if matches!(
                e.kind(),
                clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
            ) =>
        {
            None
        }
        Err(e) => Some(format!(
            "clap refuses `{filled}`: {}",
            e.to_string().lines().next().unwrap_or("")
        )),
    }
}

pub fn sha256_hex(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
