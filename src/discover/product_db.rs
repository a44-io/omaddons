//! The Battle.net Agent's `product.db`: a protobuf list of installed products and
//! discovery's best hint, since it names install paths directly, including installs on
//! other drive letters (vision §4.1, §5.2 step 1a).
//!
//! Read by field number, the layout verified on 2026-10-18 against a real Agent
//! database and two upstream readers (overtools/TACTLib
//! `TACTLib/Agent/Protobuf/ProtoDatabase.cs` at 129f977, generated from the Agent's
//! schema; lutris/lutris `lutris/util/battlenet/product_db.py` at 3307967): the
//! database's installs are its field 1 (`ProductInstall`: uid 1, product code 2,
//! settings 3 with install path 1 and game subfolder 13, cached state 4 → base state 1
//! → version string 7, product family 6). Other top-level fields (handshakes of
//! installs in progress, configs, settings) are not installs. The uid is never the
//! product: Anniversary's is `wow_classic_anniversary`, its code `wow_anniversary`.
//! Each value is also checked for its shape, so a garbage field is never a product or
//! a path. The protobuf wire reader is inline (vision §5.12): bounds-checked and
//! tolerant; a malformed message yields what decoded before it.

use std::path::Path;

use super::read_bytes;

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
    read_bytes(&product_db_path(prefix)).map_or_else(Vec::new, |bytes| parse_product_db(&bytes))
}

/// `<prefix>/drive_c/ProgramData/Battle.net/Agent/product.db`.
pub fn product_db_path(prefix: &Path) -> std::path::PathBuf {
    prefix
        .join("drive_c")
        .join("ProgramData")
        .join("Battle.net")
        .join("Agent")
        .join("product.db")
}

/// The WoW installs in a `product.db`, in file order: its top-level field 1. Never
/// fails: a malformed file yields the entries before the damage, or none.
pub fn parse_product_db(bytes: &[u8]) -> Vec<ProductDbEntry> {
    decode_message(bytes)
        .0
        .into_iter()
        .filter(|field| field.number == 1)
        .filter_map(|field| match field.value {
            Value::Bytes(install) => read_install(install),
            _ => None,
        })
        .collect()
}

/// One `ProductInstall`, kept when it has an install path and is WoW's: a `wow*`
/// product code, the product family `wow`, or a `World of Warcraft` path. A missing or
/// misshapen value is `None`.
fn read_install(install: &[u8]) -> Option<ProductDbEntry> {
    let fields = decode_message(install).0;
    let settings = sub(&fields, 3);
    let base_state = sub(&sub(&fields, 4), 1);
    let install_path = text(&settings, 1).filter(|s| is_install_path(s))?;
    let product = text(&fields, 2).filter(|s| is_product_code(s));
    let family_wow = text(&fields, 6) == Some("wow");
    let wow_path = install_path
        .to_ascii_lowercase()
        .contains("world of warcraft");
    if product.is_none() && !family_wow && !wow_path {
        return None;
    }
    Some(ProductDbEntry {
        product: product.map(str::to_owned),
        install_path: install_path.to_owned(),
        flavor_folder: text(&settings, 13)
            .filter(|s| is_flavor_folder(s))
            .map(str::to_owned),
        version: text(&base_state, 7)
            .filter(|s| is_version(s))
            .map(str::to_owned),
    })
}

/// The first field `number` of `fields` as UTF-8 text.
fn text<'a>(fields: &[Field<'a>], number: u32) -> Option<&'a str> {
    fields
        .iter()
        .find(|f| f.number == number)
        .and_then(|f| match f.value {
            Value::Bytes(bytes) => std::str::from_utf8(bytes).ok(),
            _ => None,
        })
}

/// The fields of the first field `number` of `fields` read as a message (what decodes);
/// none when it is absent or not length-delimited.
fn sub<'a>(fields: &[Field<'a>], number: u32) -> Vec<Field<'a>> {
    fields
        .iter()
        .find(|f| f.number == number)
        .and_then(|f| match f.value {
            Value::Bytes(bytes) => Some(decode_message(bytes).0),
            _ => None,
        })
        .unwrap_or_default()
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

    /// An install as the Agent writes it: uid 1, product code 2, settings 3 {path, …,
    /// game subfolder}, cached state 4 {base state {installed, version}}, operations 5,
    /// product family 6, hidden 7 and persistent JSON 8.
    fn install(uid: &str, code: &str, path: &str, folder: &str, version: &str) -> Vec<u8> {
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
        let family = if code.starts_with("wow") { "wow" } else { code };
        [
            string(1, uid),
            string(2, code),
            message(3, &settings),
            message(4, &state),
            message(5, &uint(1, 1)),
            string(6, family),
            uint(7, 0),
            string(8, "{}"),
        ]
        .concat()
    }

    fn database() -> Vec<u8> {
        [
            message(
                1,
                &install(
                    "agent",
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

    /// Anniversary's uid is `wow_classic_anniversary`, its product code
    /// `wow_anniversary`: the product is field 2, whatever field 1 holds.
    #[test]
    fn the_product_code_is_field_2_not_the_uid() {
        let db = message(
            1,
            &install(
                "wow_classic_anniversary",
                "wow_anniversary",
                "C:/Program Files (x86)/World of Warcraft",
                "_anniversary_",
                "2.5.6.70006",
            ),
        );
        let entries = parse_product_db(&db);
        assert_eq!(
            entries
                .iter()
                .map(|e| e.product.as_deref())
                .collect::<Vec<_>>(),
            [Some("wow_anniversary")],
            "{}",
            tree(&db, 0)
        );
        // A misshapen code is no product; the family and path still keep the install.
        let odd = message(
            1,
            &install(
                "wow",
                "Not A Code",
                "C:/World of Warcraft",
                "_retail_",
                "1.2.3.4",
            ),
        );
        assert_eq!(parse_product_db(&odd)[0].product, None);
    }

    /// Only top-level field 1 is an install: a handshake (field 2, product before uid)
    /// and the Agent's other fields are not, and values elsewhere in an install are not
    /// read as its path, folder or version.
    #[test]
    fn handshakes_and_other_top_level_fields_are_not_installs() {
        let path = "C:/Program Files (x86)/World of Warcraft";
        let handshake = message(
            2,
            &[
                string(1, "wow_classic_titan"),
                string(2, "wow_classic_titan"),
                message(3, &string(1, path)),
            ]
            .concat(),
        );
        let configs = message(4, &install("wow", "wow", path, "_retail_", "1.2.3.4"));
        let db = [handshake, configs, string(7, "wow")].concat();
        assert!(parse_product_db(&db).is_empty(), "{}", tree(&db, 0));
        let shuffled = [
            string(2, "wow"),
            message(9, &string(1, path)),
            message(3, &[string(2, path), string(14, "_retail_")].concat()),
        ]
        .concat();
        assert!(parse_product_db(&message(1, &shuffled)).is_empty());
    }

    /// The seed every kind of field is in: five WoW installs, the Agent's, a Titan
    /// handshake and the Agent's other fields (`tests/common/product_db.rs`).
    #[test]
    fn a_database_with_every_kind_of_field() {
        let db: &[u8] = include_bytes!("../../fuzz/seeds/product_db/multi-product.db");
        let entries = parse_product_db(db);
        assert_eq!(entries.len(), 5, "{}", tree(db, 0));
        let codes: Vec<&str> = entries
            .iter()
            .filter_map(|e| e.product.as_deref())
            .collect();
        assert_eq!(
            codes,
            [
                "wow",
                "wowf",
                "wow_classic",
                "wow_classic_era",
                "wow_anniversary"
            ]
        );
        let anniversary = &entries[4];
        assert_eq!(
            (
                anniversary.flavor_folder.as_deref(),
                anniversary.version.as_deref()
            ),
            (Some("_anniversary_"), Some("2.5.6.70006"))
        );
        assert_eq!(entries[3].install_path, "D:\\Games\\World of Warcraft");
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
        let cut = message(1, &install("wow", "wow", "C:/WoW", "_retail_", "1.2.3.4")).len();
        let wow_end = message(
            1,
            &install(
                "agent",
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
    fn deep_nesting_is_never_walked() {
        let mut db = string(1, "C:/World of Warcraft");
        for _ in 0..100 {
            db = message(1, &db);
        }
        assert!(
            parse_product_db(&db).is_empty(),
            "only settings.1 is a path, not one 100 messages down"
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
