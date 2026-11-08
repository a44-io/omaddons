//! Human rendering helpers: ANSI styling that disappears without color, an aligned
//! table formatter, and the verbosity checks that keep default output token-frugal.

use super::stderr::LogLevel;
use super::term::Style;

impl Style {
    pub fn bold(&self, text: &str) -> String {
        self.paint("1", text)
    }

    pub fn dim(&self, text: &str) -> String {
        self.paint("2", text)
    }

    /// Yellow, for warnings.
    pub fn warn(&self, text: &str) -> String {
        self.paint("33", text)
    }

    /// Red, for errors.
    pub fn error(&self, text: &str) -> String {
        self.paint("31", text)
    }

    /// Level 1 (`-v`) and above: detail fields (paths, evidence, every list item) belong
    /// in the output, and `debug` log events on stderr.
    pub fn detail(&self) -> bool {
        self.verbose >= LogLevel::Debug.min_verbosity()
    }

    /// Level 2 (`-vv`) and above: `trace` log events on stderr. Never consulted for
    /// stdout: level 2 prints exactly what level 1 prints there.
    pub fn trace(&self) -> bool {
        self.verbose >= LogLevel::Trace.min_verbosity()
    }

    fn paint(&self, code: &str, text: &str) -> String {
        if self.color {
            format!("\u{1b}[{code}m{text}\u{1b}[0m")
        } else {
            text.to_owned()
        }
    }
}

/// `1 folder`, `2 folders`: `n` and `noun`, plural (an `s`) unless `n` is 1.
pub fn count(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

/// Managed addons with the folders they own, both units named, since a folder count
/// alone reads as a count of addons (one addon can ship 22 folders): `1 addon in 22
/// folders`, `2 addons in 31 folders`, and `0 addons` with no folder count.
pub fn addons_in_folders(addons: usize, folders: u32) -> String {
    if addons == 0 {
        return count(0, "addon");
    }
    format!(
        "{} in {}",
        count(addons, "addon"),
        count(folders as usize, "folder")
    )
}

/// `18432` as `18,432`: digits grouped by three with commas, the house style for byte
/// counts in human output.
pub fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, digit) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// Rows as left-aligned columns separated by two spaces, without trailing spaces.
/// Widths count visible characters, so a colored cell aligns like a plain one.
pub fn table(rows: &[Vec<String>]) -> String {
    let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
    let widths: Vec<usize> = (0..columns)
        .map(|c| {
            rows.iter()
                .filter_map(|row| row.get(c))
                .map(|cell| visible_width(cell))
                .max()
                .unwrap_or(0)
        })
        .collect();
    let mut out = String::new();
    for row in rows {
        let mut line = String::new();
        for (c, cell) in row.iter().enumerate() {
            if c > 0 {
                line.push_str("  ");
            }
            line.push_str(cell);
            if c + 1 < row.len() {
                let width = widths.get(c).copied().unwrap_or(0);
                line.push_str(&" ".repeat(width.saturating_sub(visible_width(cell))));
            }
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

/// A cell's width on screen: its characters, color sequences (`ESC [ … m`) aside.
fn visible_width(cell: &str) -> usize {
    let mut width = 0;
    let mut chars = cell.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            // Up to and including the sequence's final letter.
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            width += 1;
        }
    }
    width
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn styling_is_a_no_op_without_color() {
        let plain = Style::default();
        assert_eq!(plain.bold("x"), "x");
        let color = Style {
            color: true,
            ..Style::default()
        };
        assert_eq!(color.bold("x"), "\u{1b}[1mx\u{1b}[0m");
        assert!(!plain.detail());
        assert!(
            Style {
                verbose: 1,
                ..Style::default()
            }
            .detail()
        );
    }

    #[test]
    fn detail_and_trace_follow_the_v_count() {
        let cases = [
            ("none", 0, false, false),
            ("v", 1, true, false),
            ("vv", 2, true, true),
            ("vvv", 3, true, true),
            ("max", u8::MAX, true, true),
        ];
        for (case, verbose, detail, trace) in cases {
            let style = Style {
                verbose,
                ..Style::default()
            };
            assert_eq!(
                (style.detail(), style.trace()),
                (detail, trace),
                "{case}: verbose {verbose} gives (detail, trace)"
            );
        }
    }

    /// Where `code` reads the `-v` count outside [`Style::detail`] and [`Style::trace`]:
    /// a `.verbose` compared with anything, any read of `style.verbose`, or the removed
    /// `paths_visible`. Each as the 0-based offset where it starts.
    fn count_reads(code: &str) -> Vec<usize> {
        let mut found = Vec::new();
        let bytes = code.as_bytes();
        for (at, _) in code.match_indices(".verbose") {
            let end = at + ".verbose".len();
            if bytes
                .get(end)
                .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_')
            {
                continue;
            }
            let rest = code[end..].trim_start();
            let compared = ["==", "!=", ">", "<"].iter().any(|op| rest.starts_with(op));
            if compared || code[..at].ends_with("style") {
                found.push(at);
            }
        }
        found.extend(code.match_indices("paths_visible").map(|(at, _)| at));
        found.sort_unstable();
        found
    }

    #[test]
    fn the_count_reader_flags_what_it_should() {
        let cases = [
            ("ctx_gt_zero", "if ctx.output.style.verbose > 0 {", true),
            ("ge_two", "let x = style.verbose >= 2;", true),
            ("eq", "a.verbose == 0", true),
            ("raw_read", "f(ctx.output.style.verbose)", true),
            ("paths_visible", "x.paths_visible()", true),
            ("detail", "style.detail()", false),
            ("copied_from_flags", "verbose: flags.verbose,", false),
            ("bool_field", "if self.verbose {", false),
            ("longer_name", "self.verbose_lines > 0", false),
        ];
        for (case, code, flagged) in cases {
            assert_eq!(
                !count_reads(code).is_empty(),
                flagged,
                "{case}: {code:?} gave {:?}",
                count_reads(code)
            );
        }
    }

    /// Only `src/output/` and `src/cli.rs` own the `-v` count; everything else asks
    /// `detail()` or `trace()`, so no detail field is gated on level 2 by accident.
    #[test]
    fn verbosity_is_read_only_through_detail_and_trace() {
        use crate::source_scan::{Literals, production_code, sources};

        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut offenders = Vec::new();
        for (path, text) in sources(&root.join("src")) {
            let rel = path.strip_prefix(root).unwrap_or(&path);
            if rel.starts_with("src/output") || rel == std::path::Path::new("src/cli.rs") {
                continue;
            }
            let Some(code) = production_code(&text, Literals::Blank) else {
                continue;
            };
            for at in count_reads(&code) {
                let line = code[..at].matches('\n').count() + 1;
                offenders.push(format!("{}:{line}", rel.display()));
            }
        }
        assert!(
            offenders.is_empty(),
            "read the -v count through Style::detail() or Style::trace(): {offenders:?}"
        );
    }

    /// A colored cell pads as its visible text does, so the next column lines up.
    #[test]
    fn colored_cells_align_like_plain_ones() {
        let color = Style {
            color: true,
            ..Style::default()
        };
        assert_eq!(visible_width(&color.warn("(missing)")), "(missing)".len());
        let rows = vec![
            vec!["a".to_owned(), color.warn("(missing)"), "/p/one".to_owned()],
            vec![
                "b".to_owned(),
                "(unregistered)".to_owned(),
                "/p/two".to_owned(),
            ],
            vec!["c".to_owned(), String::new(), "/p/three".to_owned()],
        ];
        let out = table(&rows);
        let starts: Vec<usize> = out
            .lines()
            .map(|line| {
                let at = line.find("/p/").unwrap();
                visible_width(&line[..at])
            })
            .collect();
        assert_eq!(starts, [19, 19, 19], "{out}");
    }

    #[test]
    fn tables_align_columns() {
        let rows = vec![
            vec![
                "retail".to_owned(),
                "mainline".to_owned(),
                "12.1.5".to_owned(),
            ],
            vec![
                "wotlk".to_owned(),
                "wrath-legacy".to_owned(),
                "3.3.5".to_owned(),
            ],
        ];
        assert_eq!(
            table(&rows),
            "retail  mainline      12.1.5\nwotlk   wrath-legacy  3.3.5\n"
        );
        assert_eq!(table(&[]), "");
    }

    /// Both units named, singular and plural right for 0, 1 and 2 of each; no folder
    /// count without an addon.
    #[test]
    fn addons_and_their_folders_are_both_named() {
        for (addons, folders, expected) in [
            (0, 0, "0 addons"),
            (0, 3, "0 addons"),
            (1, 1, "1 addon in 1 folder"),
            (1, 22, "1 addon in 22 folders"),
            (2, 2, "2 addons in 2 folders"),
            (5, 31, "5 addons in 31 folders"),
        ] {
            assert_eq!(
                addons_in_folders(addons, folders),
                expected,
                "{addons} addons, {folders} folders"
            );
        }
    }
}
