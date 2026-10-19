//! Human rendering helpers: ANSI styling that disappears without color, an aligned
//! table formatter, and the verbosity checks that keep default output token-frugal.

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

    /// Whether paths and other detail belong in the output (`-v`).
    pub fn paths_visible(&self) -> bool {
        self.verbose > 0
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
        assert!(!plain.paths_visible());
        assert!(
            Style {
                verbose: 1,
                ..Style::default()
            }
            .paths_visible()
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
}
