//! BPSV ("Blizzard pipe-separated values"), the format of `.build.info` and
//! `.flavor.info` (vision §4.1; reference: wowdev.wiki/TACT).
//!
//! The first line is a header of `Name!TYPE:size` columns. Columns have been added
//! over the years, so callers look them up by name (ASCII case-insensitively), never
//! by position. Values are kept verbatim: `Tags` holds spaces, `?` and `:`. Parsing is
//! tolerant: a BOM, CRLF, blank lines, `##` metadata lines, short rows and extra fields
//! are all accepted, and one bad row never rejects a file. This module reads no files;
//! callers pass text (lossy UTF-8).

/// One header column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Column {
    pub name: String,
    /// Verbatim (`STRING`, `DEC`, `HEX`, or anything newer); `STRING` when the header
    /// gives none.
    pub ty: String,
    pub size: Option<u32>,
}

/// A parsed BPSV file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bpsv {
    columns: Vec<Column>,
    rows: Vec<Vec<String>>,
}

/// One data row, read by column name.
#[derive(Debug, Clone, Copy)]
pub struct Row<'a> {
    bpsv: &'a Bpsv,
    fields: &'a [String],
}

/// Why a BPSV file has no usable header. Discovery skips the root or folder with a
/// warning.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BpsvError {
    #[error("the file has no header line")]
    Empty,
    #[error("header column {column:?} has no name")]
    BadHeader { column: String },
}

impl Bpsv {
    pub fn parse(text: &str) -> Result<Self, BpsvError> {
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        let mut lines = text
            .lines()
            .map(|line| line.strip_suffix('\r').unwrap_or(line))
            .filter(|line| !line.trim().is_empty() && !line.starts_with("##"));
        let header = lines.next().ok_or(BpsvError::Empty)?;
        let columns = header
            .split('|')
            .map(parse_column)
            .collect::<Result<Vec<_>, _>>()?;
        let rows = lines
            .map(|line| {
                line.split('|')
                    .take(columns.len())
                    .map(str::to_owned)
                    .collect()
            })
            .collect();
        Ok(Self { columns, rows })
    }

    pub fn columns(&self) -> &[Column] {
        &self.columns
    }

    /// The index of the first column named `name`, ignoring ASCII case.
    pub fn column(&self, name: &str) -> Option<usize> {
        self.columns
            .iter()
            .position(|c| c.name.eq_ignore_ascii_case(name))
    }

    pub fn rows(&self) -> impl Iterator<Item = Row<'_>> {
        self.rows.iter().map(|fields| Row { bpsv: self, fields })
    }
}

impl<'a> Row<'a> {
    /// The value of column `name` (ASCII case-insensitive); `None` when the file has no
    /// such column or this row is too short to reach it.
    pub fn get(&self, name: &str) -> Option<&'a str> {
        let index = self.bpsv.column(name)?;
        self.fields.get(index).map(String::as_str)
    }
}

fn parse_column(raw: &str) -> Result<Column, BpsvError> {
    let (name, ty, size) = match raw.split_once('!') {
        None => (raw, "STRING", None),
        Some((name, spec)) => match spec.split_once(':') {
            None => (name, spec, None),
            Some((ty, size)) => (name, ty, size.parse().ok()),
        },
    };
    if name.trim().is_empty() {
        return Err(BpsvError::BadHeader {
            column: raw.to_owned(),
        });
    }
    Ok(Column {
        name: name.to_owned(),
        ty: ty.to_owned(),
        size,
    })
}

/// The product code a `.flavor.info` names (`wow`, `wowf`, `wow_classic_era`, …):
/// the `Product Flavor` value of the first row, trimmed. The authoritative flavor
/// folder → product mapping.
pub fn flavor_product(text: &str) -> Option<String> {
    let bpsv = Bpsv::parse(text).ok()?;
    let value = bpsv.rows().next()?.get("Product Flavor")?.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

/// The columns of a `.build.info` row that discovery uses. Absent and empty values are
/// both `None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildInfoRow {
    pub product: Option<String>,
    pub version: Option<String>,
    /// `Active` is `1`.
    pub active: bool,
    pub branch: Option<String>,
    pub tags: Option<String>,
}

/// Every product row of a `.build.info`.
pub fn build_info_rows(text: &str) -> Result<Vec<BuildInfoRow>, BpsvError> {
    let bpsv = Bpsv::parse(text)?;
    let value =
        |row: &Row<'_>, name: &str| row.get(name).filter(|v| !v.is_empty()).map(str::to_owned);
    Ok(bpsv
        .rows()
        .map(|row| BuildInfoRow {
            product: value(&row, "Product"),
            version: value(&row, "Version"),
            active: row.get("Active") == Some("1"),
            branch: value(&row, "Branch"),
            tags: value(&row, "Tags"),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    /// The header of a current client, from the vision.
    const HEADER: &str = "Branch!STRING:0|Active!DEC:1|Build Key!HEX:16|CDN Key!HEX:16|Install Key!HEX:16|IM Size!DEC:4|CDN Path!STRING:0|CDN Hosts!STRING:0|CDN Servers!STRING:0|Tags!STRING:0|Armadillo!STRING:0|Last Activated!STRING:0|Version!STRING:0|KeyRing!HEX:16|Product!STRING:0";
    const TAGS: &str = "Windows x86_64 US? acct-USA? geoip-US? enUS speech?:Windows x86_64 US? acct-USA? geoip-US? enUS text?";

    fn build_info() -> String {
        format!(
            "{HEADER}\n\
             us|1|0123456789abcdef0123456789abcdef|fedcba9876543210fedcba9876543210|00112233445566778899aabbccddeeff|8|tpr/wow|us.cdn.blizzard.com level3.blizzard.com|http://us.cdn.blizzard.com/?maxhosts=4|{TAGS}||2026-10-01T12:00:00Z|12.1.5.70391|3ca57fe7319a297346440e4d2a03a0cd|wow\n\
             eu|0|0123456789abcdef0123456789abcdef|fedcba9876543210fedcba9876543210|00112233445566778899aabbccddeeff|8|tpr/wow|eu.cdn.blizzard.com|http://eu.cdn.blizzard.com/?maxhosts=4|Windows x86_64 EU? enGB speech?||2026-09-01T12:00:00Z|5.5.4.63587|3ca57fe7319a297346440e4d2a03a0cd|wow_classic\n"
        )
    }

    #[test]
    fn the_vision_header_reads_by_name() {
        let text = build_info();
        let bpsv = Bpsv::parse(&text).unwrap();
        assert_eq!(bpsv.columns().len(), 15);
        assert_eq!(
            bpsv.columns()[2],
            Column {
                name: "Build Key".into(),
                ty: "HEX".into(),
                size: Some(16)
            }
        );
        assert_eq!(bpsv.columns()[14].name, "Product");
        let rows: Vec<Row<'_>> = bpsv.rows().collect();
        assert_eq!(rows.len(), 2);
        let get = |row: usize, name: &str| rows[row].get(name);
        assert_eq!(
            (get(0, "Product"), get(0, "Version"), get(0, "Active")),
            (Some("wow"), Some("12.1.5.70391"), Some("1"))
        );
        assert_eq!(get(0, "Tags"), Some(TAGS));
        assert_eq!(get(0, "product"), Some("wow"), "names ignore ASCII case");
        assert_eq!(get(0, "Armadillo"), Some(""));
        assert_eq!(get(0, "Nope"), None);
        assert_eq!(
            build_info_rows(&text).unwrap(),
            [
                BuildInfoRow {
                    product: Some("wow".into()),
                    version: Some("12.1.5.70391".into()),
                    active: true,
                    branch: Some("us".into()),
                    tags: Some(TAGS.into()),
                },
                BuildInfoRow {
                    product: Some("wow_classic".into()),
                    version: Some("5.5.4.63587".into()),
                    active: false,
                    branch: Some("eu".into()),
                    tags: Some("Windows x86_64 EU? enGB speech?".into()),
                },
            ]
        );
    }

    #[test]
    fn older_and_reordered_layouts_read_by_name() {
        let reordered = "Version!STRING:0|Product!STRING:0|Active!DEC:1\n12.1.5.70391|wow|1\n";
        let rows = build_info_rows(reordered).unwrap();
        assert_eq!(
            (
                rows[0].version.as_deref(),
                rows[0].product.as_deref(),
                rows[0].active
            ),
            (Some("12.1.5.70391"), Some("wow"), true)
        );
        let older = "Branch!STRING:0|Active!DEC:1|Build Key!HEX:16|Tags!STRING:0|Version!STRING:0\n\
                     us|1|00ff|Windows x86_64 US?|3.3.5.12340\n";
        let rows = build_info_rows(older).unwrap();
        assert_eq!(
            (
                rows[0].version.as_deref(),
                rows[0].active,
                rows[0].product.as_deref()
            ),
            (Some("3.3.5.12340"), true, None)
        );
    }

    #[test]
    fn column_specs() {
        let bpsv = Bpsv::parse("Odd!FOO:3|Plain|NoSize!DEC|Bad!HEX:x\n").unwrap();
        let columns: Vec<(&str, &str, Option<u32>)> = bpsv
            .columns()
            .iter()
            .map(|c| (c.name.as_str(), c.ty.as_str(), c.size))
            .collect();
        assert_eq!(
            columns,
            [
                ("Odd", "FOO", Some(3)),
                ("Plain", "STRING", None),
                ("NoSize", "DEC", None),
                ("Bad", "HEX", None),
            ]
        );
        assert_eq!(
            Bpsv::parse("A!STRING:0|!DEC:1\n"),
            Err(BpsvError::BadHeader {
                column: "!DEC:1".into()
            })
        );
        assert_eq!(Bpsv::parse(""), Err(BpsvError::Empty));
        assert_eq!(Bpsv::parse("\n## seqn = 1\n  \n"), Err(BpsvError::Empty));
    }

    #[test]
    fn tolerated_noise() {
        let text = "\u{feff}## seqn = 7\r\nA!STRING:0|B!STRING:0|C!STRING:0\r\n\r\nx|y|z\r\nshort\r\n## comment\r\nl|o|n|g|extra\r\n\r\n";
        let bpsv = Bpsv::parse(text).unwrap();
        assert_eq!(bpsv.columns()[0].name, "A", "BOM stripped");
        let rows: Vec<(Option<&str>, Option<&str>, Option<&str>)> = bpsv
            .rows()
            .map(|r| (r.get("A"), r.get("B"), r.get("C")))
            .collect();
        assert_eq!(
            rows,
            [
                (Some("x"), Some("y"), Some("z")),
                (Some("short"), None, None),
                (Some("l"), Some("o"), Some("n")),
            ]
        );
        let lone_cr = Bpsv::parse("A!STRING:0\nvalue\r").unwrap();
        assert_eq!(lone_cr.rows().next().unwrap().get("A"), Some("value"));
    }

    #[test]
    fn flavor_info() {
        for text in [
            "Product Flavor!STRING:0\nwowf\n",
            "Product Flavor!STRING:0\nwowf",
            "Product Flavor!STRING:0\r\nwowf \r\n",
        ] {
            assert_eq!(flavor_product(text).as_deref(), Some("wowf"), "{text:?}");
        }
        assert_eq!(flavor_product("Product Flavor!STRING:0\n"), None);
        assert_eq!(flavor_product("Product Flavor!STRING:0\n  \n"), None);
        assert_eq!(flavor_product("Other!STRING:0\nwow\n"), None);
        assert_eq!(flavor_product(""), None);
    }

    #[test]
    fn no_filesystem_access() {
        let source = include_str!("bpsv.rs");
        assert!(!source.contains(concat!("std::", "fs")));
        assert!(!source.contains(concat!(" fs", "::")));
    }

    /// Distinct column names (case-insensitively) and rows of values without `|` or
    /// line breaks; each row's first value is non-blank and not a `##` line.
    fn table() -> impl Strategy<Value = (Vec<String>, Vec<Vec<String>>)> {
        proptest::collection::btree_set("[a-z][a-z ]{0,8}", 1..6).prop_flat_map(|names| {
            let names: Vec<String> = names.into_iter().collect();
            let width = names.len();
            let row = (
                "[a-z0-9][a-zA-Z0-9 ?:._/-]{0,11}",
                proptest::collection::vec("[a-zA-Z0-9 ?:._/-]{0,12}", width - 1),
            )
                .prop_map(|(first, rest)| std::iter::once(first).chain(rest).collect());
            (Just(names), proptest::collection::vec(row, 0..5))
        })
    }

    proptest! {
        #[test]
        fn round_trips_by_name((names, rows) in table(), crlf in any::<bool>()) {
            let eol = if crlf { "\r\n" } else { "\n" };
            let header: Vec<String> = names.iter().map(|n| format!("{n}!STRING:0")).collect();
            let mut text = header.join("|") + eol;
            for row in &rows {
                text.push_str(&row.join("|"));
                text.push_str(eol);
            }
            let bpsv = match Bpsv::parse(&text) {
                Ok(bpsv) => bpsv,
                Err(err) => return Err(TestCaseError::fail(format!("{text:?}: {err}"))),
            };
            let parsed: Vec<Row<'_>> = bpsv.rows().collect();
            prop_assert_eq!(parsed.len(), rows.len(), "{:?}", text);
            for (row, expected) in parsed.iter().zip(&rows) {
                for (name, value) in names.iter().zip(expected) {
                    prop_assert_eq!(row.get(&name.to_uppercase()), Some(value.as_str()), "{:?}", text);
                }
            }
        }

        #[test]
        fn never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..512)) {
            let text = String::from_utf8_lossy(&bytes);
            let _ = Bpsv::parse(&text);
            let _ = flavor_product(&text);
            let _ = build_info_rows(&text);
        }

        #[test]
        fn never_panics_on_bpsv_like_text(text in "[a-z!:|#0-9 \r\n\u{feff}]{0,200}") {
            if let Ok(bpsv) = Bpsv::parse(&text) {
                for row in bpsv.rows() {
                    for column in bpsv.columns() {
                        let _ = row.get(&column.name);
                    }
                }
            }
            let _ = flavor_product(&text);
            let _ = build_info_rows(&text);
        }
    }
}
