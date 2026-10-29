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
    let mut at = match doc.strip_prefix(FENCE) {
        Some(rest) if rest.starts_with('\n') => FENCE.len() + 1,
        _ => return Err(StampError::NoFrontmatter),
    };
    // The closing fence: the first later line that is exactly `---`.
    loop {
        if at >= doc.len() {
            return Err(StampError::UnclosedFrontmatter);
        }
        let rest = &doc[at..];
        let (line, next) = match rest.find('\n') {
            Some(i) => (&rest[..i], at + i + 1),
            None => (rest, doc.len()),
        };
        at = next;
        if line == FENCE {
            break;
        }
    }
    let rest = &doc[at..];
    if !rest.starts_with(PREFIX) {
        return Err(StampError::NoStampLine);
    }
    let Some(newline) = rest.find('\n') else {
        return Err(StampError::MalformedStamp);
    };
    let digits = rest[..newline]
        .strip_prefix(PREFIX)
        .and_then(|line| line.strip_suffix(SUFFIX))
        .filter(|digits| {
            digits.len() == DIGITS
                && digits
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
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
    let located = locate(doc)?;
    let mut hasher = Sha256::new();
    hasher.update(&doc.as_bytes()[..located.start]);
    hasher.update(&doc.as_bytes()[located.end..]);
    Ok(hasher.finalize().into())
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
}
