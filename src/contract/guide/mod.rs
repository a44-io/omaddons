//! The agent guide (M4-D7): one Markdown document, hand-written prose from `guide.md`
//! with blocks generated from the registries, rendered byte-stable. `robot-docs guide`
//! prints it, `skills/omaddons/SKILL.md` is a copy `tests/skill.rs` holds equal to it,
//! and `install-skill` writes the same bytes.

pub mod render;
pub mod skill_file;
pub mod stamp;

/// The guide's template, edited by hand; everything else in the guide is generated.
pub const TEMPLATE: &str = include_str!("guide.md");

/// The guide as this build renders it.
pub fn render_guide() -> Result<String, render::RenderError> {
    render::render(TEMPLATE, &render::GuideInputs::live())
}

/// The digest the committed `skills/omaddons/SKILL.md` carries without its stamp line,
/// computed by `build.rs` without rendering (M4-D7): 64 lowercase hex digits. The skill
/// test holds it equal to the stamp of the guide this binary renders; the doctor's
/// skill-drift check compares installed skills with it. No block reads it: the guide
/// never depends on its own digest.
pub const EXPECTED_DIGEST: &str = env!("OMADDONS_GUIDE_SHA256");

#[cfg(test)]
mod tests {
    use super::render::{Kind, PLACEHOLDERS};
    use super::*;
    use crate::app::COMMANDS;

    fn guide() -> String {
        render_guide().unwrap()
    }

    #[test]
    fn expected_digest_is_sixty_four_lowercase_hex_digits() {
        let hex = EXPECTED_DIGEST;
        assert!(
            hex.len() == 64
                && hex
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "EXPECTED_DIGEST is {hex:?}"
        );
    }

    #[test]
    fn expected_digest_is_the_committed_skills_digest() {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("skills/omaddons/SKILL.md");
        let text = std::fs::read_to_string(&path).unwrap();
        let digest = stamp::hex(&stamp::digest_without_stamp(&text).unwrap());
        assert!(
            digest.eq(EXPECTED_DIGEST),
            "the committed skill's digest is {digest}, EXPECTED_DIGEST is {EXPECTED_DIGEST}: rebuild, build.rs reads the committed file"
        );
    }

    /// The template's lines after its frontmatter.
    fn body_lines() -> Vec<&'static str> {
        let lines: Vec<&str> = TEMPLATE.lines().collect();
        let close = lines
            .iter()
            .skip(1)
            .position(|line| *line == "---")
            .map_or(0, |i| i + 2);
        lines[close..].to_vec()
    }

    #[test]
    fn the_embedded_template_renders() {
        let rendered = render_guide();
        assert!(rendered.is_ok(), "{rendered:?}");
    }

    #[test]
    fn the_template_holds_every_placeholder() {
        let lines: Vec<&str> = TEMPLATE.lines().collect();
        let mut missing = Vec::new();
        let mut duplicated = Vec::new();
        for placeholder in PLACEHOLDERS {
            let written = format!("{{{{{}}}}}", placeholder.name);
            match placeholder.kind {
                Kind::Block => match lines.iter().filter(|line| **line == written).count() {
                    0 => missing.push(placeholder.name),
                    1 => {}
                    _ => duplicated.push(placeholder.name),
                },
                Kind::Inline => {
                    if !TEMPLATE.contains(&written) {
                        missing.push(placeholder.name);
                    }
                }
            }
        }
        assert_eq!(
            (missing, duplicated),
            (vec![], vec![]),
            "(missing, duplicated) placeholders"
        );
    }

    #[test]
    fn blocks_stand_in_their_own_paragraphs() {
        let lines: Vec<&str> = TEMPLATE.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            // The stamp stands on the line after the frontmatter, not in a paragraph.
            let block = PLACEHOLDERS
                .iter()
                .filter(|p| p.kind == Kind::Block && !matches!(p.name, "stamp"))
                .any(|p| {
                    line.strip_prefix("{{").and_then(|l| l.strip_suffix("}}")) == Some(p.name)
                });
            if block {
                let before = i.checked_sub(1).and_then(|j| lines.get(j)).copied();
                let after = lines.get(i + 1).copied();
                assert_eq!(
                    (before, after),
                    (Some(""), Some("")),
                    "line {}: {line} has a blank line around it",
                    i + 1
                );
            }
        }
    }

    #[test]
    fn the_guide_opens_with_frontmatter_then_stamp() {
        let guide = guide();
        let lines: Vec<&str> = guide.lines().collect();
        assert_eq!(&lines[..2], ["---", "name: omaddons"], "{guide}");
        assert!(lines[2].starts_with("description: "), "{}", lines[2]);
        assert_eq!(lines[3], "---");
        assert!(lines[4].starts_with(stamp::PREFIX), "{}", lines[4]);
        assert_eq!(stamp::verifies(&guide), Ok(true), "the stamp verifies");
    }

    #[test]
    fn the_guide_is_plain_lf_markdown() {
        let guide = guide();
        assert!(
            !guide.contains('\r') && !guide.contains('\t'),
            "LF only, no tab"
        );
        for (i, line) in guide.lines().enumerate() {
            assert_eq!(line.trim_end(), line, "line {}: trailing whitespace", i + 1);
        }
        assert!(
            guide.ends_with('\n') && !guide.ends_with("\n\n"),
            "one final newline"
        );
        assert!(!guide.contains("{{"), "no placeholder left");
    }

    #[test]
    fn the_holding_prose_names_only_commands_that_exist() {
        // Each backticked span of the body that starts with `omaddons ` names a COMMANDS
        // path: the longest run of its words that is one. The frontmatter's description is
        // the plan's trigger sentence, describing M4's whole surface.
        let mut unknown = Vec::new();
        for line in body_lines() {
            for span in line.split('`').skip(1).step_by(2) {
                let Some(rest) = span.strip_prefix("omaddons ") else {
                    continue;
                };
                let words: Vec<&str> = rest
                    .split(' ')
                    .take_while(|w| !w.starts_with('-') && !w.starts_with('<'))
                    .collect();
                let names_a_row = (1..=words.len())
                    .any(|n| COMMANDS.iter().any(|row| row.name == words[..n].join(" ")));
                if !names_a_row {
                    unknown.push(span.to_owned());
                }
            }
        }
        assert_eq!(
            unknown,
            Vec::<String>::new(),
            "commands the build does not have"
        );
    }
}
