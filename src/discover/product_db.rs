//! The Battle.net Agent's `product.db`: a protobuf list of installed products and
//! discovery's best hint, since it names install paths directly, including installs on
//! other drive letters (vision §4.1, §5.2 step 1a).
//!
//! The field layout (WowUp's reader) is unverified, so extraction is by shape, not by
//! field number: each top-level message is one candidate install, and its strings are
//! classified as an install path, a `wow*` product code, a four-part version or a
//! `_flavor_` folder. The protobuf wire reader is inline (vision §5.12): bounds-checked,
//! depth-limited and tolerant; a malformed message yields what decoded before it.

use std::path::Path;

use super::read_bytes;

/// Nested messages deeper than this are not walked.
const MAX_DEPTH: usize = 16;

/// One WoW install that `product.db` lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductDbEntry {
    /// E.g. `wow`; `None` when the entry is kept for its `World of Warcraft` path.
    pub product: Option<String>,
    /// A Windows path, e.g. `C:/Program Files (x86)/World of Warcraft`.
    pub install_path: String,
    /// E.g. `_retail_`.
    pub flavor_folder: Option<String>,
    /// E.g. `12.1.0.69933`.
    pub version: Option<String>,
}

/// The WoW installs of `<prefix>/drive_c/ProgramData/Battle.net/Agent/product.db`;
/// empty when it is missing, too large or unreadable.
pub fn read_product_db(prefix: &Path) -> Vec<ProductDbEntry> {
    let path = prefix
        .join("drive_c")
        .join("ProgramData")
        .join("Battle.net")
        .join("Agent")
        .join("product.db");
    read_bytes(&path).map_or_else(Vec::new, |bytes| parse_product_db(&bytes))
}

/// The WoW installs in a `product.db`, in file order. Never fails: a malformed file
/// yields the entries before the damage, or none.
pub fn parse_product_db(bytes: &[u8]) -> Vec<ProductDbEntry> {
    decode_message(bytes)
        .0
        .into_iter()
        .filter_map(|field| match field.value {
            Value::Bytes(entry) => classify(entry),
            _ => None,
        })
        .collect()
}

fn classify(entry: &[u8]) -> Option<ProductDbEntry> {
    let mut strings = Vec::new();
    walk(entry, 1, &mut strings);
    let first = |test: fn(&str) -> bool| strings.iter().copied().find(|s| test(s));
    let install_path = first(is_install_path)?;
    let product = first(is_product_code);
    let wow_path = install_path
        .to_ascii_lowercase()
        .contains("world of warcraft");
    if product.is_none() && !wow_path {
        return None;
    }
    Some(ProductDbEntry {
        product: product.map(str::to_owned),
        install_path: install_path.to_owned(),
        flavor_folder: first(is_flavor_folder).map(str::to_owned),
        version: first(is_version).map(str::to_owned),
    })
}

/// Collects the printable strings in a message, depth first. A length-delimited value
/// is read both ways: as a string when it is printable UTF-8, and as a nested message
/// when it decodes cleanly to at least one field.
fn walk<'a>(message: &'a [u8], depth: usize, out: &mut Vec<&'a str>) {
    if depth > MAX_DEPTH {
        return;
    }
    let (fields, _) = decode_message(message);
    for field in fields {
        let Value::Bytes(bytes) = field.value else {
            continue;
        };
        if let Ok(text) = std::str::from_utf8(bytes)
            && !text.is_empty()
            && !text.chars().any(char::is_control)
        {
            out.push(text);
        }
        let (nested, clean) = decode_message(bytes);
        if clean && !nested.is_empty() {
            walk(bytes, depth + 1, out);
        }
    }
}

/// `C:/…` or `C:\…`.
fn is_install_path(s: &str) -> bool {
    let b = s.as_bytes();
    matches!(b, [letter, b':', b'/' | b'\\', ..] if letter.is_ascii_alphabetic())
}

/// Every WoW product code starts with `wow`.
fn is_product_code(s: &str) -> bool {
    s.starts_with("wow")
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// `12.1.0.69933`.
fn is_version(s: &str) -> bool {
    let parts: Vec<&str> = s.split('.').collect();
    parts.len() == 4
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

/// `_retail_`.
fn is_flavor_folder(s: &str) -> bool {
    s.len() > 2
        && s.starts_with('_')
        && s.ends_with('_')
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

// ---------------------------------------------------------------------------------------
// The protobuf wire format.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Value<'a> {
    Varint(u64),
    Fixed64(u64),
    Bytes(&'a [u8]),
    Fixed32(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Field<'a> {
    number: u32,
    value: Value<'a>,
}

/// The fields of one message, and whether the whole buffer decoded cleanly. Decoding
/// stops at the first malformed field (bad varint, truncation, a group or an unknown
/// wire type, field number 0).
fn decode_message(buf: &[u8]) -> (Vec<Field<'_>>, bool) {
    let mut fields = Vec::new();
    let mut pos = 0;
    while pos < buf.len() {
        match field(buf, &mut pos) {
            Some(f) => fields.push(f),
            None => return (fields, false),
        }
    }
    (fields, true)
}

fn field<'a>(buf: &'a [u8], pos: &mut usize) -> Option<Field<'a>> {
    let key = varint(buf, pos)?;
    let number = u32::try_from(key >> 3).ok().filter(|n| *n != 0)?;
    let value = match key & 7 {
        0 => Value::Varint(varint(buf, pos)?),
        1 => Value::Fixed64(u64::from_le_bytes(take(buf, pos, 8)?.try_into().ok()?)),
        2 => {
            let len = usize::try_from(varint(buf, pos)?).ok()?;
            Value::Bytes(take(buf, pos, len)?)
        }
        5 => Value::Fixed32(u32::from_le_bytes(take(buf, pos, 4)?.try_into().ok()?)),
        _ => return None,
    };
    Some(Field { number, value })
}

fn take<'a>(buf: &'a [u8], pos: &mut usize, len: usize) -> Option<&'a [u8]> {
    let end = pos.checked_add(len)?;
    let bytes = buf.get(*pos..end)?;
    *pos = end;
    Some(bytes)
}

/// A base-128 varint of at most 10 bytes; `None` when truncated or over 64 bits.
fn varint(buf: &[u8], pos: &mut usize) -> Option<u64> {
    let mut value = 0u64;
    for shift in (0..64).step_by(7) {
        let byte = *buf.get(*pos)?;
        *pos += 1;
        if shift == 63 && byte > 1 {
            return None;
        }
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some(value);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    /// The development machine's product.db: product codes, Windows install paths,
    /// versions, public build-config hashes and region/locale tags; no account data.
    const RETAIL_DB: &[u8] = include_bytes!("../../tests/fixtures/product_db/retail.db");

    fn put_varint(mut n: u64, out: &mut Vec<u8>) {
        loop {
            let low = (n & 0x7f) as u8;
            n >>= 7;
            if n == 0 {
                out.push(low);
                return;
            }
            out.push(low | 0x80);
        }
    }

    fn message(number: u32, body: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        put_varint(u64::from(number) << 3 | 2, &mut out);
        put_varint(body.len() as u64, &mut out);
        out.extend_from_slice(body);
        out
    }

    fn string(number: u32, s: &str) -> Vec<u8> {
        message(number, s.as_bytes())
    }

    fn uint(number: u32, n: u64) -> Vec<u8> {
        let mut out = Vec::new();
        put_varint(u64::from(number) << 3, &mut out);
        put_varint(n, &mut out);
        out
    }

    /// A WowUp-shaped install: uid, product code, settings {path, …, flavor folder},
    /// cached state {base state {installed, version}}.
    fn install(code: &str, path: &str, folder: &str, version: &str) -> Vec<u8> {
        let settings = [
            string(1, path),
            string(2, "us"),
            uint(3, 2),
            string(6, "enUS"),
            message(8, &[string(1, "enUS"), uint(2, 3)].concat()),
            string(11, "USA"),
            string(13, folder),
        ]
        .concat();
        let base = [
            uint(1, 1),
            uint(2, 1),
            string(7, version),
            string(12, "dcfc90ff"),
        ]
        .concat();
        let state = message(1, &base);
        [
            string(1, code),
            string(2, code),
            message(3, &settings),
            message(4, &state),
            string(6, code),
        ]
        .concat()
    }

    fn database() -> Vec<u8> {
        [
            message(
                1,
                &install(
                    "agent",
                    "C:/ProgramData/Battle.net/Agent",
                    "",
                    "2.41.1.9824",
                ),
            ),
            message(
                1,
                &install(
                    "wow",
                    "C:/Program Files (x86)/World of Warcraft",
                    "_retail_",
                    "12.1.0.69933",
                ),
            ),
            message(
                1,
                &install(
                    "wow_classic_era",
                    "D:\\Games\\World of Warcraft",
                    "_classic_era_",
                    "1.15.9.65000",
                ),
            ),
            message(5, &[uint(1, 0), uint(2, 1_000_000)].concat()),
            uint(6, 4_072_387),
            string(7, "wow"),
        ]
        .concat()
    }

    fn retail() -> ProductDbEntry {
        ProductDbEntry {
            product: Some("wow".into()),
            install_path: "C:/Program Files (x86)/World of Warcraft".into(),
            flavor_folder: Some("_retail_".into()),
            version: Some("12.1.0.69933".into()),
        }
    }

    /// The decoded field tree, for assertion messages.
    fn tree(buf: &[u8], depth: usize) -> String {
        let mut out = String::new();
        for f in decode_message(buf).0 {
            out.push_str(&format!(
                "{}{}: {:?}\n",
                "  ".repeat(depth),
                f.number,
                f.value
            ));
            if let Value::Bytes(b) = f.value
                && decode_message(b).1
                && depth < 4
            {
                out.push_str(&tree(b, depth + 1));
            }
        }
        out
    }

    #[test]
    fn a_wowup_shaped_database() {
        let db = database();
        let entries = parse_product_db(&db);
        assert_eq!(entries.len(), 2, "{}", tree(&db, 0));
        assert_eq!(entries[0], retail());
        assert_eq!(
            entries[1],
            ProductDbEntry {
                product: Some("wow_classic_era".into()),
                install_path: "D:\\Games\\World of Warcraft".into(),
                flavor_folder: Some("_classic_era_".into()),
                version: Some("1.15.9.65000".into()),
            }
        );
    }

    #[test]
    fn the_development_machine_database() {
        assert_eq!(
            parse_product_db(RETAIL_DB),
            [retail()],
            "{}",
            tree(RETAIL_DB, 0)
        );
    }

    #[test]
    fn extraction_ignores_field_numbers_and_depth() {
        let shuffled = [
            string(9, "wow"),
            message(2, &message(7, &message(3, &string(40, "12.1.0.69933")))),
            message(
                15,
                &[
                    string(2, "_retail_"),
                    string(1_000, "C:/Program Files (x86)/World of Warcraft"),
                ]
                .concat(),
            ),
        ]
        .concat();
        let db = message(3, &shuffled);
        assert_eq!(parse_product_db(&db), [retail()], "{}", tree(&db, 0));
    }

    #[test]
    fn non_wow_entries_are_dropped_and_wow_paths_kept() {
        let other = message(
            1,
            &[
                string(1, "pro"),
                message(3, &string(1, "C:/Games/Overwatch")),
            ]
            .concat(),
        );
        let pathless = message(1, &[string(1, "wow"), string(2, "12.1.0.69933")].concat());
        let unnamed = message(1, &message(3, &string(1, "E:/World of Warcraft")).clone());
        let entries = parse_product_db(&[other, pathless, unnamed].concat());
        assert_eq!(
            entries,
            [ProductDbEntry {
                product: None,
                install_path: "E:/World of Warcraft".into(),
                flavor_folder: None,
                version: None,
            }]
        );
    }

    #[test]
    fn damage_keeps_what_decoded_before_it() {
        let db = database();
        assert_eq!(
            parse_product_db(&db[..db.len() - 3]).len(),
            2,
            "trailing damage"
        );
        let cut = message(1, &install("wow", "C:/WoW", "_retail_", "1.2.3.4")).len();
        let wow_end = message(
            1,
            &install(
                "agent",
                "C:/ProgramData/Battle.net/Agent",
                "",
                "2.41.1.9824",
            ),
        )
        .len()
            + cut;
        assert_eq!(
            parse_product_db(&db[..wow_end - 1]).len(),
            0,
            "a truncated entry is dropped"
        );
        assert!(parse_product_db(&[]).is_empty());
        assert!(
            parse_product_db(&[0x0b, 0x01]).is_empty(),
            "groups end parsing"
        );
        assert!(
            parse_product_db(&[0x00, 0x01]).is_empty(),
            "field 0 is malformed"
        );
    }

    #[test]
    fn varints() {
        let read = |bytes: &[u8]| varint(bytes, &mut 0);
        assert_eq!(read(&[0x00]), Some(0));
        assert_eq!(read(&[0xac, 0x02]), Some(300));
        let max = [0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01];
        assert_eq!(read(&max), Some(u64::MAX));
        let over = [0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x02];
        assert_eq!(read(&over), None, "more than 64 bits");
        assert_eq!(read(&[0x80; 11]), None, "more than 10 bytes");
        assert_eq!(read(&[0x80]), None, "truncated");
        let mut buf = Vec::new();
        put_varint(u64::MAX, &mut buf);
        assert_eq!(read(&buf), Some(u64::MAX));
    }

    #[test]
    fn deep_nesting_stops_at_the_limit() {
        let mut db = string(1, "C:/World of Warcraft");
        for _ in 0..100 {
            db = message(1, &db);
        }
        assert!(
            parse_product_db(&db).is_empty(),
            "the path is below the depth limit"
        );
    }

    #[test]
    fn reads_from_the_prefix() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_product_db(dir.path()).is_empty());
        let agent = dir.path().join("drive_c/ProgramData/Battle.net/Agent");
        std::fs::create_dir_all(&agent).unwrap();
        std::fs::write(agent.join("product.db"), RETAIL_DB).unwrap();
        assert_eq!(read_product_db(dir.path()), [retail()]);
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(10_000))]

        #[test]
        fn arbitrary_bytes_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..256)) {
            let _ = parse_product_db(&bytes);
        }

        #[test]
        fn mutated_databases_never_panic(
            edits in proptest::collection::vec((any::<prop::sample::Index>(), any::<u8>()), 1..8),
            cut in any::<prop::sample::Index>(),
        ) {
            let mut db = database();
            for (at, byte) in edits {
                let i = at.index(db.len());
                db[i] = byte;
            }
            let end = cut.index(db.len() + 1);
            let first = parse_product_db(&db[..end]);
            prop_assert_eq!(&first, &parse_product_db(&db[..end]), "deterministic");
        }
    }
}
