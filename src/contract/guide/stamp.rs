//! The guide's stamp (M4-D7, M4-D50): the line right after the frontmatter,
//! `<!-- omaddons guide sha256:<hex> -->`, whose digest covers the document with that one
//! line omitted. One algorithm for the renderer, `build.rs`, `install-skill` and the
//! doctor's skill-drift check, so this file uses only `std` and `sha2`: `build.rs`
//! compiles it with `#[path]`, outside the crate.

use sha2::{Digest, Sha256};

pub const PREFIX: &str = "<!-- omaddons guide sha256:";
pub const SUFFIX: &str = " -->";

/// The frontmatter's opening and closing line.
const FENCE: &str = "---";
/// The stamp's digits: SHA-256, lowercase hex.
const DIGITS: usize = 64;

/// Why a document has no stamp to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StampError {
    /// It does not begin with the line `---` (a byte-order mark or a CRLF fence included).
    NoFrontmatter,
    /// No later line is exactly `---`.
    UnclosedFrontmatter,
    /// The line after the closing fence is missing or does not start with [`PREFIX`].
    NoStampLine,
    /// It starts with [`PREFIX`] but is not [`PREFIX`], 64 lowercase hex digits and
    /// [`SUFFIX`] followed by a newline.
    MalformedStamp,
}

impl std::fmt::Display for StampError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NoFrontmatter => "the document does not begin with a `---` line",
            Self::UnclosedFrontmatter => "the frontmatter has no closing `---` line",
            Self::NoStampLine => "the line after the frontmatter is not an omaddons guide stamp",
            Self::MalformedStamp => "the guide stamp after the frontmatter is malformed",
        })
    }
}

impl std::error::Error for StampError {}

/// Where a document's stamp line is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Located {
    /// The stamp line's first byte.
    pub start: usize,
    /// One past its `\n`.
    pub end: usize,
    /// Its 64 digits.
    pub recorded: String,
}

/// The stamp line of `doc`: the line right after the frontmatter's closing fence.
pub fn locate(doc: &str) -> Result<Located, StampError> {
    locate_bytes(doc.as_bytes())
}

/// [`locate`] over bytes, compared exactly (no trimming, no decoding): a document whose
/// bytes after the stamp line are not UTF-8 (a skill edited by another tool) still has
/// its stamp read.
pub fn locate_bytes(doc: &[u8]) -> Result<Located, StampError> {
    let fence = FENCE.as_bytes();
    let mut at = match doc.strip_prefix(fence) {
        Some(rest) if rest.first() == Some(&b'\n') => fence.len() + 1,
        _ => return Err(StampError::NoFrontmatter),
    };
    // The closing fence: the first later line that is exactly `---`.
    loop {
        if at >= doc.len() {
            return Err(StampError::UnclosedFrontmatter);
        }
        let rest = &doc[at..];
        let (line, next) = match rest.iter().position(|&b| b == b'\n') {
            Some(i) => (&rest[..i], at + i + 1),
            None => (rest, doc.len()),
        };
        at = next;
        if line == fence {
            break;
        }
    }
    let rest = &doc[at..];
    if !rest.starts_with(PREFIX.as_bytes()) {
        return Err(StampError::NoStampLine);
    }
    let Some(newline) = rest.iter().position(|&b| b == b'\n') else {
        return Err(StampError::MalformedStamp);
    };
    let digits = rest[..newline]
        .strip_prefix(PREFIX.as_bytes())
        .and_then(|line| line.strip_suffix(SUFFIX.as_bytes()))
        .filter(|digits| {
            digits.len() == DIGITS
                && digits
                    .iter()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
        })
        .and_then(|digits| std::str::from_utf8(digits).ok())
        .ok_or(StampError::MalformedStamp)?;
    Ok(Located {
        start: at,
        end: at + newline + 1,
        recorded: digits.to_owned(),
    })
}

/// SHA-256 of `doc` without its stamp line (and that line's `\n`). The recorded digits
/// play no part.
pub fn digest_without_stamp(doc: &str) -> Result<[u8; 32], StampError> {
    digest_bytes_without_stamp(doc.as_bytes())
}

/// [`digest_without_stamp`] over bytes: the one digest the renderer stamps, `build.rs`
/// computes and [`inspect`] compares.
pub fn digest_bytes_without_stamp(doc: &[u8]) -> Result<[u8; 32], StampError> {
    let located = locate_bytes(doc)?;
    Ok(omitting(doc, &located))
}

/// SHA-256 of `doc` with the stamp line `located` names omitted.
fn omitting(doc: &[u8], located: &Located) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(&doc[..located.start]);
    hasher.update(&doc[located.end..]);
    hasher.finalize().into()
}

/// What a skill's bytes say about the guide that produced them (M4-D50): the stamp a
/// guide carries, and whether the bytes are still the ones it was computed over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stamp {
    /// The stamp line is where it belongs: what it claims, and the digest of the bytes
    /// with that line omitted.
    Stamped { claimed: [u8; 32], actual: [u8; 32] },
    /// No frontmatter, no closing fence, or no well-formed stamp on the line after it.
    Unstamped,
}

impl Stamp {
    /// Bytes some guide produced, unedited: the stamp matches its own document.
    pub fn is_pristine(&self) -> bool {
        self.digest().is_some()
    }

    /// The guide digest of a pristine file; `None` for an edited or unstamped one.
    pub fn digest(&self) -> Option<[u8; 32]> {
        match self {
            Self::Stamped { claimed, actual } if claimed == actual => Some(*actual),
            _ => None,
        }
    }
}

/// The stamp of `bytes`, exactly as [`locate_bytes`] finds it, and the digest the
/// renderer would have stamped over the same bytes. Total and pure: it never panics,
/// copies nothing and reads nothing.
pub fn inspect(bytes: &[u8]) -> Stamp {
    let Ok(located) = locate_bytes(bytes) else {
        return Stamp::Unstamped;
    };
    let Some(claimed) = unhex(&located.recorded) else {
        return Stamp::Unstamped;
    };
    Stamp::Stamped {
        claimed,
        actual: omitting(bytes, &located),
    }
}

/// The digest 64 lowercase hex digits spell.
fn unhex(hex: &str) -> Option<[u8; 32]> {
    let digits = hex.as_bytes();
    if digits.len() != DIGITS {
        return None;
    }
    let value = |b: u8| match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        _ => None,
    };
    let mut out = [0u8; 32];
    for (byte, pair) in out.iter_mut().zip(digits.chunks_exact(2)) {
        *byte = (value(pair[0])? << 4) | value(pair[1])?;
    }
    Some(out)
}

/// Whether `doc`'s recorded digits are its digest.
pub fn verifies(doc: &str) -> Result<bool, StampError> {
    let recorded = locate(doc)?.recorded;
    let computed = hex(&digest_without_stamp(doc)?);
    Ok(computed.eq(&recorded))
}

/// 64 lowercase hex digits.
pub fn hex(digest: &[u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(DIGITS);
    for byte in digest {
        out.push(char::from(HEX[usize::from(byte >> 4)]));
        out.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    out
}

/// The stamp line for `hex`, without its newline.
pub fn line(hex: &str) -> String {
    format!("{PREFIX}{hex}{SUFFIX}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const ZEROS: &str = "0000000000000000000000000000000000000000000000000000000000000000";

    fn doc(stamp: &str) -> String {
        format!("---\nname: omaddons\n---\n{stamp}\n# Body\n\ntext\n")
    }

    fn stamped(body: &str) -> String {
        let provisional = format!("---\nname: x\n---\n{}\n{body}", line(ZEROS));
        let digest = digest_without_stamp(&provisional).unwrap();
        provisional.replacen(ZEROS, &hex(&digest), 1)
    }

    #[test]
    fn locate_cases() {
        let valid = doc(&line(ZEROS));
        let start = "---\nname: omaddons\n---\n".len();
        let cases: Vec<(&str, String, Result<Located, StampError>)> = vec![
            (
                "valid",
                valid.clone(),
                Ok(Located {
                    start,
                    end: start + line(ZEROS).len() + 1,
                    recorded: ZEROS.to_owned(),
                }),
            ),
            (
                "no_frontmatter",
                "# Body\n".to_owned(),
                Err(StampError::NoFrontmatter),
            ),
            (
                "byte_order_mark",
                format!("\u{feff}{valid}"),
                Err(StampError::NoFrontmatter),
            ),
            (
                "crlf_fence",
                valid.replacen("---\n", "---\r\n", 1),
                Err(StampError::NoFrontmatter),
            ),
            (
                "unclosed",
                "---\nname: x\n".to_owned(),
                Err(StampError::UnclosedFrontmatter),
            ),
            (
                "fence_with_trailing_space",
                format!("---\nname: x\n--- \n{}\n", line(ZEROS)),
                Err(StampError::UnclosedFrontmatter),
            ),
            (
                "eof_after_fence",
                "---\nname: x\n---\n".to_owned(),
                Err(StampError::NoStampLine),
            ),
            (
                "eof_fence_without_newline",
                "---\nname: x\n---".to_owned(),
                Err(StampError::NoStampLine),
            ),
            (
                "other_line_after_fence",
                doc("# Title"),
                Err(StampError::NoStampLine),
            ),
            (
                "stamp_without_newline",
                format!("---\nname: x\n---\n{}", line(ZEROS)),
                Err(StampError::MalformedStamp),
            ),
            (
                "uppercase_hex",
                doc(&line(&"A".repeat(64))),
                Err(StampError::MalformedStamp),
            ),
            (
                "short_hex",
                doc(&line(&"0".repeat(63))),
                Err(StampError::MalformedStamp),
            ),
            (
                "long_hex",
                doc(&line(&"0".repeat(65))),
                Err(StampError::MalformedStamp),
            ),
            (
                "trailing_space",
                doc(&format!("{} ", line(ZEROS))),
                Err(StampError::MalformedStamp),
            ),
        ];
        for (case, input, want) in cases {
            let got = locate(&input);
            assert_eq!(
                got, want,
                "{case}: input {input:?} gave {got:?}, expected {want:?}"
            );
        }
    }

    #[test]
    fn digest_omits_exactly_the_stamp_line() {
        let document = doc(&line(ZEROS));
        let without = document.replacen(&format!("{}\n", line(ZEROS)), "", 1);
        let want: [u8; 32] = Sha256::digest(without.as_bytes()).into();
        assert_eq!(
            digest_without_stamp(&document),
            Ok(want),
            "the stamp line and its newline are omitted"
        );
        let other = document.replacen(ZEROS, &"7".repeat(64), 1);
        assert_eq!(
            digest_without_stamp(&other),
            Ok(want),
            "the recorded digits play no part"
        );
        let edits = [
            (
                "frontmatter",
                document.replacen("name: omaddons", "name: omaddonz", 1),
            ),
            ("body", document.replacen("# Body", "# Bodx", 1)),
            ("last_byte", format!("{}!", &document[..document.len() - 1])),
            (
                "final_newline_removed",
                document[..document.len() - 1].to_owned(),
            ),
        ];
        for (case, edited) in edits {
            assert_ne!(
                digest_without_stamp(&edited),
                Ok(want),
                "{case}: an edit changes the digest"
            );
        }
    }

    #[test]
    fn verifies_cases() {
        let fresh = stamped("# Body\n");
        assert_eq!(verifies(&fresh), Ok(true), "freshly stamped");
        assert_eq!(
            verifies(&fresh.replacen("Body", "Bodx", 1)),
            Ok(false),
            "one byte edited"
        );
        let other = stamped("# Other\n");
        let swapped = fresh.replacen(
            &locate(&fresh).unwrap().recorded,
            &locate(&other).unwrap().recorded,
            1,
        );
        assert_eq!(verifies(&swapped), Ok(false), "another document's stamp");
        assert_eq!(
            verifies("# no frontmatter\n"),
            Err(StampError::NoFrontmatter),
            "malformed"
        );
    }

    #[test]
    fn hex_and_line_format() {
        let digest: [u8; 32] = Sha256::digest(b"omaddons").into();
        let h = hex(&digest);
        assert_eq!(h.len(), 64, "{h}");
        assert!(
            h.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "{h}"
        );
        assert_eq!(hex(&[0xab; 32]), "ab".repeat(32));
        assert_eq!(line(&h), format!("<!-- omaddons guide sha256:{h} -->"));
    }

    // -----------------------------------------------------------------------------------
    // `inspect`: an installed skill's bytes (M4-D50).

    use proptest::prelude::*;

    fn rendered() -> String {
        crate::contract::guide::render_guide().unwrap()
    }

    /// `text`'s length and first three lines, for a failure message.
    fn shown(text: &[u8]) -> String {
        let lossy = String::from_utf8_lossy(text);
        let head: Vec<&str> = lossy.lines().take(3).collect();
        format!("{} bytes, starting {head:?}", text.len())
    }

    /// `doc` with its stamp recomputed over its own bytes: what the renderer would print.
    fn restamped(doc: &str) -> String {
        let located = locate(doc).unwrap();
        let digest = digest_without_stamp(doc).unwrap();
        format!(
            "{}{}\n{}",
            &doc[..located.start],
            line(&hex(&digest)),
            &doc[located.end..]
        )
    }

    #[test]
    fn the_rendered_guide_is_stamped_and_current() {
        let guide = rendered();
        let stamp = inspect(guide.as_bytes());
        let Stamp::Stamped { claimed, actual } = stamp else {
            panic!("the guide is stamped: {}", shown(guide.as_bytes())); // ubs:ignore — the test's own failure
        };
        assert_eq!(claimed, actual, "its stamp is its own digest");
        assert!(stamp.is_pristine());
        let expected = unhex(crate::contract::guide::EXPECTED_DIGEST).unwrap();
        assert_eq!(stamp.digest(), Some(expected), "the build's digest");
        assert_eq!(
            Ok(actual),
            digest_without_stamp(&guide),
            "the renderer's own digest function"
        );
    }

    #[test]
    fn an_edited_guide_is_not_pristine() {
        let guide = rendered();
        let at_description = guide.find("description: ").unwrap() + "description: ".len();
        let body = locate(&guide).unwrap().end + 10;
        let bytes = |edit: &dyn Fn(&mut Vec<u8>)| {
            let mut copy = guide.as_bytes().to_vec();
            edit(&mut copy);
            copy
        };
        let cases: Vec<(&str, Vec<u8>)> = vec![
            (
                "a_byte_in_the_description",
                bytes(&|b| b[at_description] ^= 0x01),
            ),
            (
                "a_byte_in_the_last_line",
                bytes(&|b| {
                    let last = b.len() - 2;
                    b[last] ^= 0x01;
                }),
            ),
            (
                "the_trailing_newline_removed",
                bytes(&|b| {
                    b.pop();
                }),
            ),
            (
                "a_line_appended",
                bytes(&|b| b.extend_from_slice(b"my own note\n")),
            ),
            ("a_body_character_changed", bytes(&|b| b[body] ^= 0x01)),
            ("a_byte_that_is_not_utf8", bytes(&|b| b[body] = 0xff)),
        ];
        for (case, edited) in cases {
            assert_ne!(
                edited,
                guide.as_bytes(),
                "{case}: the edit changed something"
            );
            let stamp = inspect(&edited);
            assert!(
                matches!(stamp, Stamp::Stamped { .. }) && !stamp.is_pristine(),
                "{case}: {stamp:?} for {}",
                shown(&edited)
            );
            assert_eq!(stamp.digest(), None, "{case}");
        }
    }

    #[test]
    fn another_guide_with_its_own_stamp_is_pristine() {
        let guide = rendered();
        let other = restamped(&format!("{guide}A prose line another guide has.\n"));
        let stamp = inspect(other.as_bytes());
        assert!(
            stamp.is_pristine(),
            "{stamp:?} for {}",
            shown(other.as_bytes())
        );
        let expected = unhex(crate::contract::guide::EXPECTED_DIGEST).unwrap();
        assert_ne!(stamp.digest(), Some(expected), "another guide's digest");
        assert_eq!(verifies(&other), Ok(true), "B1's check agrees");
    }

    #[test]
    fn malformed_stamps_are_unstamped() {
        let valid = stamped("# Body\n");
        let stamp_line = valid.lines().nth(3).unwrap().to_owned();
        let digits = locate(&valid).unwrap().recorded;
        let cases: Vec<(&str, String)> = vec![
            ("empty", String::new()),
            ("no_frontmatter", "# Body\n".to_owned()),
            ("unclosed_frontmatter", "---\nname: x\n# Body\n".to_owned()),
            (
                "stamp_on_line_one",
                format!("{stamp_line}\n---\nname: x\n---\n"),
            ),
            (
                "blank_line_before_stamp",
                valid.replacen("---\n<!--", "---\n\n<!--", 1),
            ),
            (
                "uppercase_hex",
                valid.replacen(&digits, &digits.to_uppercase(), 1),
            ),
            ("short_hex", valid.replacen(&digits, &digits[1..], 1)),
            (
                "long_hex",
                valid.replacen(&digits, &format!("{digits}0"), 1),
            ),
            ("trailing_space", valid.replacen(" -->\n", " --> \n", 1)),
            ("crlf_line_ends", valid.replace('\n', "\r\n")),
            (
                "stamp_without_newline_at_eof",
                format!("---\nname: x\n---\n{stamp_line}"),
            ),
            (
                "html_comment_of_another_tool",
                "---\nname: x\n---\n<!-- generated by another tool -->\n# Body\n".to_owned(),
            ),
        ];
        assert!(
            inspect(valid.as_bytes()).is_pristine(),
            "the base case is valid"
        );
        for (case, text) in cases {
            assert_eq!(
                inspect(text.as_bytes()),
                Stamp::Unstamped,
                "{case}: {}",
                shown(text.as_bytes())
            );
        }
    }

    #[test]
    fn bytes_and_text_locate_alike() {
        for text in [
            stamped("# Body\n"),
            doc(&line(ZEROS)),
            "---\nname: x\n".to_owned(),
            "# Body\n".to_owned(),
        ] {
            assert_eq!(locate_bytes(text.as_bytes()), locate(&text), "{text:?}");
            assert_eq!(
                digest_bytes_without_stamp(text.as_bytes()),
                digest_without_stamp(&text),
                "{text:?}"
            );
        }
    }

    proptest! {
        #[test]
        fn inspect_is_total(bytes in proptest::collection::vec(any::<u8>(), 0..512)) {
            let stamp = inspect(&bytes);
            prop_assert_eq!(matches!(stamp, Stamp::Stamped { .. }), locate_bytes(&bytes).is_ok());
        }

        #[test]
        fn inspect_after_a_frontmatter_is_total(
            stamp_digits in "[0-9a-fA-F]{60,68}",
            tail in proptest::collection::vec(any::<u8>(), 0..256),
        ) {
            let mut bytes = format!("---\nname: x\n---\n{PREFIX}{stamp_digits}{SUFFIX}\n").into_bytes();
            bytes.extend_from_slice(&tail);
            let stamp = inspect(&bytes);
            let well_formed = stamp_digits.len() == DIGITS
                && stamp_digits.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
            prop_assert_eq!(matches!(stamp, Stamp::Stamped { .. }), well_formed);
        }
    }
}
