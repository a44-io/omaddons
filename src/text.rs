//! Release notes as plain text: GitHub's Atom HTML and WoWInterface's BBCode, turned
//! into bounded text with no markup, safe for terminals, agents and the widget; and one
//! line of a source's text (a name, author or version) made safe the same way
//! ([`source_line`]).
//!
//! Every function is a pure, infallible, linear-time state machine (no regex, no HTML
//! parser). The changelog functions' output never holds `<` before an ASCII letter, `/`,
//! `!` or `?`, never a known BBCode tag, and no control character but `\n` and `\t` (so
//! a changelog cannot send terminal escapes); a source line holds no control character
//! at all. Links are only ever text. Output is at most `max_chars + 1` characters:
//! longer text is cut on a character boundary and ends with `…`.

/// The BBCode tags WoWInterface's editor knows, matched ignoring case: these are
/// dropped (their content kept); any other bracketed word (`[Fixed]`) is prose.
pub const BBCODE_TAGS: &[&str] = &[
    "b",
    "i",
    "u",
    "s",
    "strike",
    "color",
    "size",
    "font",
    "highlight",
    "left",
    "center",
    "right",
    "indent",
    "url",
    "email",
    "img",
    "video",
    "youtube",
    "list",
    "quote",
    "code",
    "php",
    "html",
    "noparse",
    "spoiler",
    "hr",
    "table",
    "tr",
    "td",
    "th",
    "sub",
    "sup",
    "attach",
    "*",
];

/// The longest BBCode tag (name and value) looked at; a longer `[` is text.
const BBCODE_TAG_MAX: usize = 2048;

/// The longest entity looked at (`&#x10FFFF;` is 10).
const ENTITY_MAX: usize = 12;

/// HTML as plain text, at most `max_chars + 1` characters.
///
/// Tags and comments are stripped; `<br>`, `</p>` and `</div>` (and the ends of lists
/// and tables) break lines; `<li>` starts a `- ` line; headings get their own lines;
/// `<pre>` and `<code>` keep their spacing; `<script>` and `<style>` content is dropped;
/// `&lt; &gt; &amp; &quot; &#39; &apos; &nbsp; &#N; &#xH;` decode, other entities stay
/// as written. A `<` that is not a tag (`a < b`) is text.
pub fn html_to_text(input: &str, max_chars: usize) -> String {
    let chars: Vec<char> = input.chars().collect();
    let mut out = Out::default();
    let mut pre = 0usize;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '<' && chars.get(i + 1).is_some_and(|n| tag_start(*n)) {
            if starts_with_ci(&chars, i, "<!--") {
                i = find(&chars, i + 4, "-->").map_or(chars.len(), |end| end + 3);
                continue;
            }
            let Some(end) = tag_end(&chars, i) else {
                // Unterminated: the rest is the tag.
                break;
            };
            let (closing, name) = tag_name(&chars[i + 1..end]);
            i = end + 1;
            match (closing, name.as_str()) {
                (false, "script" | "style") => {
                    let close = format!("</{name}");
                    i = find_ci(&chars, i, &close)
                        .and_then(|at| tag_end(&chars, at))
                        .map_or(chars.len(), |gt| gt + 1);
                }
                (_, "br") => out.newline(),
                (true, "p" | "div" | "ul" | "ol" | "table" | "tr" | "blockquote") => {
                    out.newline();
                }
                (false, "li") => {
                    out.line_start();
                    out.push_str("- ");
                }
                (false, h) if heading(h) => out.line_start(),
                (true, h) if heading(h) => out.newline(),
                // `<pre>` is a block of its own; `<code>` may sit inside a line.
                (false, "pre") => {
                    out.line_start();
                    pre += 1;
                }
                (true, "pre") => {
                    pre = pre.saturating_sub(1);
                    out.newline();
                }
                (false, "code") => pre += 1,
                (true, "code") => pre = pre.saturating_sub(1),
                _ => {}
            }
            continue;
        }
        if c == '&'
            && let Some((decoded, len)) = entity(&chars[i..])
        {
            i += len;
            out.text(decoded, pre > 0);
            continue;
        }
        out.text(c, pre > 0);
        i += 1;
    }
    finish(out.text, max_chars, false)
}

/// Whether [`source_line`] decodes HTML entities.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Entities {
    /// Decoded: WoWInterface's API HTML-escapes its text.
    Decode,
    /// Kept as written: a JSON API that does not escape (a literal `&amp;` stays).
    Keep,
}

/// One line of a source's untrusted text (a name, author or version), safe to show and
/// to match: entities decoded with [`Entities::Decode`] (a decoded character is plain
/// text, never read again); every control character, and every character that reorders
/// or hides text, dropped; line breaks and tabs as spaces, whitespace runs as one;
/// trimmed and cut to `max_chars` (then `…`).
pub fn source_line(input: &str, max_chars: usize, entities: Entities) -> String {
    let chars: Vec<char> = input.chars().collect();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    while i < chars.len() {
        let (c, len) = match chars[i] {
            '&' if entities == Entities::Decode => entity(&chars[i..]).unwrap_or(('&', 1)),
            c => (c, 1),
        };
        i += len;
        if c.is_whitespace() || matches!(c, '\u{2028}' | '\u{2029}') {
            if !out.is_empty() && !out.ends_with(' ') {
                out.push(' ');
            }
        } else if !c.is_control() && !hides_text(c) {
            out.push(c);
        }
    }
    truncate(out.trim(), max_chars)
}

/// The format characters that reorder or hide text: zero-width characters and marks,
/// bidirectional embeddings, overrides and isolates, invisible operators, the BOM.
fn hides_text(c: char) -> bool {
    matches!(
        c,
        '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{2069}'
            | '\u{FEFF}'
    )
}

/// BBCode as plain text, at most `max_chars + 1` characters.
///
/// CRLF and lone CRs become LF; `[*]` starts a `- ` line; `[url=X]Y[/url]` (or
/// `[url="X"]`) becomes `Y (X)`; every other known tag ([`BBCODE_TAGS`]), opening or
/// closing, is dropped and its content kept. Unknown bracketed words stay.
pub fn bbcode_to_text(input: &str, max_chars: usize) -> String {
    let chars: Vec<char> = input.chars().collect();
    let mut out = String::new();
    // Each open `[url]`'s target, and where its text starts.
    let mut urls: Vec<(Option<String>, usize)> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '['
            && let Some((tag, len)) = bbcode_tag(&chars[i..])
        {
            i += len;
            match (tag.closing, tag.name.as_str()) {
                (false, "*") => {
                    if !out.is_empty() && !out.ends_with('\n') {
                        out.push('\n');
                    }
                    out.push_str("- ");
                }
                (false, "url") => urls.push((tag.value, out.len())),
                (true, "url") => {
                    if let Some((Some(target), start)) = urls.pop() {
                        let text = out.get(start..).unwrap_or_default().trim();
                        if text.is_empty() {
                            out.truncate(start);
                            out.push_str(&target);
                        } else if text != target {
                            out.push_str(&format!(" ({target})"));
                        }
                    }
                }
                (true, "list") | (false, "hr") if !out.ends_with('\n') => out.push('\n'),
                _ => {}
            }
            continue;
        }
        // WoWInterface's changelogs are HTML-escaped BBCode: an entity is its character,
        // as text (never read again as a tag).
        if c == '&'
            && let Some((decoded, len)) = entity(&chars[i..])
        {
            i += len;
            if !decoded.is_control() || matches!(decoded, '\n' | '\t') {
                out.push(decoded);
            }
            continue;
        }
        match c {
            '\r' => {
                out.push('\n');
                if chars.get(i + 1) == Some(&'\n') {
                    i += 1;
                }
            }
            c if c.is_control() && c != '\n' && c != '\t' => {}
            c => out.push(c),
        }
        i += 1;
    }
    finish(out, max_chars, true)
}

/// Output being built for HTML: whitespace collapses outside `<pre>`, and line breaks
/// never pile up into more than one blank line.
#[derive(Default)]
struct Out {
    text: String,
}

impl Out {
    fn push_str(&mut self, s: &str) {
        self.text.push_str(s);
    }

    /// A character of text; `verbatim` inside `<pre>` and `<code>`.
    fn text(&mut self, c: char, verbatim: bool) {
        if verbatim {
            match c {
                '\r' => {}
                c if c.is_control() && c != '\n' && c != '\t' => {}
                c => self.text.push(c),
            }
            return;
        }
        if c.is_whitespace() {
            if !self.text.is_empty() && !self.text.ends_with([' ', '\n']) {
                self.text.push(' ');
            }
        } else if !c.is_control() {
            self.text.push(c);
        }
    }

    /// A line break, at most one blank line in a row.
    fn newline(&mut self) {
        let trimmed = self.text.trim_end_matches(' ').len();
        self.text.truncate(trimmed);
        if !self.text.is_empty() && !self.text.ends_with("\n\n") {
            self.text.push('\n');
        }
    }

    /// A break unless at the start of a line.
    fn line_start(&mut self) {
        if !self.text.is_empty() && !self.text.ends_with('\n') {
            self.newline();
        }
    }
}

/// The common end: control characters dropped, blank-line runs collapsed (for BBCode,
/// whose text is preformatted), markup that stripping or decoding re-created defanged,
/// trimmed and cut. Controls go first: dropping one later could join `<` to a letter
/// (a `[url=…]` target is copied out raw).
fn finish(text: String, max_chars: usize, collapse: bool) -> String {
    let text: String = text
        .chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .collect();
    let text = if collapse {
        collapse_blank_lines(&text)
    } else {
        text
    };
    truncate(defang(&text).trim(), max_chars)
}

/// Lines with trailing spaces trimmed, at most one blank line in a row.
fn collapse_blank_lines(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut blank = 0;
    for line in text.split('\n') {
        let line = line.trim_end();
        if line.is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
        } else {
            blank = 0;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Turns markup-shaped text back into prose: a known BBCode tag's brackets become
/// parentheses, then `<` before a letter, `/`, `!` or `?` becomes `&lt;` (neither pass
/// can make what the other removes).
fn defang(text: &str) -> String {
    let brackets = defang_brackets(text);
    let chars: Vec<char> = brackets.chars().collect();
    let mut out = String::with_capacity(brackets.len());
    for (i, c) in chars.iter().enumerate() {
        if *c == '<' && chars.get(i + 1).is_some_and(|n| tag_start(*n)) {
            out.push_str("&lt;");
        } else {
            out.push(*c);
        }
    }
    out
}

fn defang_brackets(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '['
            && let Some((_, len)) = bbcode_tag(&chars[i..])
        {
            out.push('(');
            for inner in chars.iter().skip(i + 1).take(len.saturating_sub(2)) {
                out.push(match inner {
                    '[' => '(',
                    ']' => ')',
                    other => *other,
                });
            }
            out.push(')');
            i += len;
            continue;
        } else {
            out.push(c);
        }
        i += 1;
    }
    out
}

/// At most `max_chars` characters, else the first `max_chars` (trailing whitespace
/// trimmed) and `…`.
fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    let cut: String = text.chars().take(max_chars).collect();
    let mut cut = cut.trim_end().to_owned();
    cut.push('…');
    cut
}

/// Whether `text` holds markup either function removes: a `<` before an ASCII letter,
/// `/`, `!` or `?`, or a complete known BBCode tag (`[b]`, `[/url]`, `[url=x]`). Their
/// output never does.
pub fn has_markup(text: &str) -> bool {
    let chars: Vec<char> = text.chars().collect();
    chars.iter().enumerate().any(|(i, c)| {
        (*c == '<' && chars.get(i + 1).is_some_and(|n| tag_start(*n)))
            || (*c == '[' && bbcode_tag(&chars[i..]).is_some())
    })
}

/// What may follow `<` for it to open a tag.
const fn tag_start(c: char) -> bool {
    c.is_ascii_alphabetic() || matches!(c, '/' | '!' | '?')
}

const fn heading(name: &str) -> bool {
    matches!(name.as_bytes(), [b'h', b'1'..=b'6'])
}

/// The `>` closing the tag that starts at `start`, outside quoted attribute values.
fn tag_end(chars: &[char], start: usize) -> Option<usize> {
    let mut quote: Option<char> = None;
    for (i, c) in chars.iter().enumerate().skip(start + 1) {
        match (quote, *c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(*c),
            (None, '>') => return Some(i),
            (None, _) => {}
        }
    }
    None
}

/// Whether a tag closes, and its lowercase name.
fn tag_name(inner: &[char]) -> (bool, String) {
    let closing = inner.first() == Some(&'/');
    let name = inner
        .iter()
        .skip(usize::from(closing))
        .take_while(|c| c.is_ascii_alphanumeric())
        .map(char::to_ascii_lowercase)
        .collect();
    (closing, name)
}

/// The first `needle` at or after `from`.
fn find(chars: &[char], from: usize, needle: &str) -> Option<usize> {
    let needle: Vec<char> = needle.chars().collect();
    (from..chars.len()).find(|&i| chars.get(i..i + needle.len()) == Some(needle.as_slice()))
}

/// [`find`], ignoring ASCII case.
fn find_ci(chars: &[char], from: usize, needle: &str) -> Option<usize> {
    (from..chars.len()).find(|&i| starts_with_ci(chars, i, needle))
}

fn starts_with_ci(chars: &[char], at: usize, needle: &str) -> bool {
    let mut n = 0;
    for (k, want) in needle.chars().enumerate() {
        match chars.get(at + k) {
            Some(c) if c.eq_ignore_ascii_case(&want) => n += 1,
            _ => return false,
        }
    }
    n > 0
}

/// The character an entity at the start of `chars` stands for, and its length; `None`
/// leaves it as written.
fn entity(chars: &[char]) -> Option<(char, usize)> {
    let end = chars.iter().take(ENTITY_MAX).position(|c| *c == ';')?;
    let body: String = chars.get(1..end)?.iter().collect();
    let decoded = match body.as_str() {
        "lt" => '<',
        "gt" => '>',
        "amp" => '&',
        "quot" => '"',
        "apos" | "#39" => '\'',
        "nbsp" => ' ',
        numeric => {
            let digits = numeric.strip_prefix('#')?;
            let code = match digits.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => digits.parse::<u32>().ok()?,
            };
            char::from_u32(code)?
        }
    };
    Some((decoded, end + 1))
}

/// A BBCode tag.
struct BbTag {
    closing: bool,
    /// Lowercase.
    name: String,
    value: Option<String>,
}

/// The known BBCode tag at the start of `chars` (`[name]`, `[name=value]`,
/// `[name="value"]`, `[/name]`), and its length.
fn bbcode_tag(chars: &[char]) -> Option<(BbTag, usize)> {
    let close = chars.iter().take(BBCODE_TAG_MAX).position(|c| *c == ']')?;
    let inner = chars.get(1..close)?;
    let closing = inner.first() == Some(&'/');
    let body = &inner[usize::from(closing)..];
    let name_len = body
        .iter()
        .take_while(|c| c.is_ascii_alphanumeric() || **c == '*')
        .count();
    let name: String = body[..name_len]
        .iter()
        .map(char::to_ascii_lowercase)
        .collect();
    if !BBCODE_TAGS.contains(&name.as_str()) {
        return None;
    }
    let rest: String = body[name_len..].iter().collect();
    let value = match rest.strip_prefix('=') {
        _ if closing && !rest.is_empty() => return None,
        Some(v) => Some(v.trim().trim_matches('"').to_owned()),
        None if rest.is_empty() => None,
        // `[b x]` is not a tag.
        None => return None,
    };
    Some((
        BbTag {
            closing,
            name,
            value,
        },
        close + 1,
    ))
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    const MAX: usize = 4000;

    fn html(s: &str) -> String {
        html_to_text(s, MAX)
    }

    fn bb(s: &str) -> String {
        bbcode_to_text(s, MAX)
    }

    #[test]
    fn html_tags_entities_and_blocks() {
        assert_eq!(
            html("<p>Hello <b>world</b></p><p>Two</p>"),
            "Hello world\nTwo"
        );
        assert_eq!(html("a<br>b<br/>c"), "a\nb\nc");
        assert_eq!(
            html("<ul><li>one</li><li>two <i>2</i></li></ul>after"),
            "- one\n- two 2\nafter"
        );
        assert_eq!(html("intro<h2>Title</h2>body"), "intro\nTitle\nbody");
        assert_eq!(html("x <!-- hidden --> y"), "x y");
        assert_eq!(html("a < b and c > d"), "a < b and c > d");
        assert_eq!(
            html("&lt;b&gt; &amp; &quot;q&quot; &#39;s&#39; &#65;&#x42; &bogus; &nbsp;x"),
            "&lt;b> & \"q\" 's' AB &bogus; x"
        );
        assert_eq!(html("<a href=\"x>y\" title='>'>link</a> text"), "link text");
        assert_eq!(html("<div>a</div><div>b</div>"), "a\nb");
        assert_eq!(
            html("a<script>alert('<b>')</script>b<style>p{}</style>c"),
            "abc"
        );
        assert_eq!(html("<pre>  keep\n    this  </pre>"), "keep\n    this");
        assert_eq!(
            html("<pre><span>if</span> x  then\n\n\n  y</pre>"),
            "if x  then\n\n\n  y"
        );
        assert_eq!(html("<p>a</p>\n\n\n<p>b</p><br><br><br><br>c"), "a\nb\n\nc");
        assert_eq!(html("text <b unterminated"), "text");
        assert_eq!(html("x<script>never closed"), "x");
        assert_eq!(html("bell \u{7}&#27;[31m red"), "bell [31m red");
    }

    #[test]
    fn decoded_entities_never_rebuild_markup() {
        assert_eq!(
            html("&lt;Frame&gt; and &lt;/b&gt;"),
            "&lt;Frame> and &lt;/b>"
        );
        assert_eq!(html("<<b>b>"), "&lt;b>");
        assert_eq!(html("1 &lt; 2"), "1 < 2");
    }

    #[test]
    fn bbcode_tags_links_lists_and_lines() {
        assert_eq!(bb("[b]Bold[/b] and [COLOR=red]red[/COLOR]"), "Bold and red");
        assert_eq!(
            bb("[url=https://x.org/a]the site[/url]"),
            "the site (https://x.org/a)"
        );
        assert_eq!(bb("[URL=\"https://x.org\"]x[/URL]"), "x (https://x.org)");
        assert_eq!(bb("[url]https://x.org[/url]"), "https://x.org");
        assert_eq!(bb("[url=https://x.org][/url]"), "https://x.org");
        assert_eq!(
            bb("[list]\r\n[*]one\r\n[*]two\r\n[/list]after"),
            "- one\n- two\nafter"
        );
        assert_eq!(
            bb("line one\r\nline two\rline three"),
            "line one\nline two\nline three"
        );
        assert_eq!(
            bb("[Fixed] a crash [WIP] [b x]"),
            "[Fixed] a crash [WIP] [b x]"
        );
        assert_eq!(bb("a\n\n\n\nb   \n"), "a\n\nb");
        assert_eq!(bb("[[b]b]x"), "(b)x");
        assert_eq!(bb("<b>raw html</b>"), "&lt;b>raw html&lt;/b>");
        // A control character in a link target is dropped before markup is defanged
        // (found by fuzzing).
        assert_eq!(
            bb("[url=https://x.org/Bug<\u{0}\u{0}Sack]log[/url]"),
            "log (https://x.org/Bug&lt;Sack)"
        );
    }

    #[test]
    fn truncation_cuts_on_a_character_and_marks_it() {
        assert_eq!(html_to_text("abcdef", 6), "abcdef");
        assert_eq!(html_to_text("abcdefg", 6), "abcdef…");
        assert_eq!(html_to_text("abcde", 6), "abcde");
        assert_eq!(bbcode_to_text("ééééé", 3), "ééé…");
        assert_eq!(bbcode_to_text("abc", 0), "…");
        assert_eq!(html_to_text("", 0), "");
        assert_eq!(html_to_text("ab cd", 3), "ab…");
    }

    fn markupish() -> impl Strategy<Value = String> {
        proptest::collection::vec(
            prop_oneof![
                Just("<".to_owned()),
                Just(">".to_owned()),
                Just("</".to_owned()),
                Just("<b>".to_owned()),
                Just("<pre>".to_owned()),
                Just("</pre>".to_owned()),
                Just("<li>".to_owned()),
                Just("<script>".to_owned()),
                Just("&lt;".to_owned()),
                Just("&#27;".to_owned()),
                Just("&#x3c;".to_owned()),
                Just("[".to_owned()),
                Just("]".to_owned()),
                Just("[b]".to_owned()),
                Just("[/url]".to_owned()),
                Just("[url=".to_owned()),
                Just("[*]".to_owned()),
                Just("\"".to_owned()),
                Just("\r\n".to_owned()),
                Just("\u{1b}".to_owned()),
                Just("\u{0}".to_owned()),
                Just("é".to_owned()),
                "[a-z ]{0,6}",
            ],
            0..60,
        )
        .prop_map(|parts| parts.concat())
    }

    proptest! {
        #[test]
        fn outputs_are_bounded_plain_and_safe(input in markupish(), max in 0usize..80) {
            for out in [html_to_text(&input, max), bbcode_to_text(&input, max)] {
                let n = out.chars().count();
                prop_assert!(n <= max + 1, "{n} > {max} + 1: {out:?}");
                if n == max + 1 {
                    prop_assert!(out.ends_with('…'), "{out:?}");
                }
                prop_assert!(!has_markup(&out), "markup in {out:?} from {input:?}");
                prop_assert!(
                    out.chars().all(|c| !c.is_control() || c == '\n' || c == '\t'),
                    "control character in {out:?}"
                );
            }
        }

        #[test]
        fn arbitrary_text_never_panics(input in "\\PC{0,300}", max in 0usize..400) {
            let _ = html_to_text(&input, max);
            let _ = bbcode_to_text(&input, max);
        }

        /// A source line never holds a control or text-hiding character, nor a line
        /// break, and keeps its bound, whatever comes in (raw or as entities).
        #[test]
        fn a_source_line_is_always_safe(input in "(\\PC|[\\x00-\\x1f\\x7f-\\x9f\\u{202e}\\u{200b}&#;x])*", max in 1usize..64) {
            for entities in [Entities::Decode, Entities::Keep] {
                let out = source_line(&input, max, entities);
                prop_assert!(
                    out.chars().all(|c| !c.is_control() && !hides_text(c)),
                    "{out:?} from {input:?}"
                );
                prop_assert!(out.chars().count() <= max + 1);
                prop_assert_eq!(out.trim(), out.as_str());
            }
        }
    }

    #[test]
    fn source_lines_decode_once_and_drop_what_a_terminal_obeys() {
        for (input, expected) in [
            (
                "DBM &#54392;&#52852; (DBM-VPPukaKR)",
                "DBM \u{d478}\u{ce74} (DBM-VPPukaKR)",
            ),
            (
                "DBM Voicepack &quot;Yike&quot; &#x41;",
                "DBM Voicepack \"Yike\" A",
            ),
            ("Some&amp;One", "Some&One"),
            // Once: an escaped entity stays an entity.
            ("&amp;amp;", "&amp;"),
            // An escape as an entity or raw, C1 included: gone.
            ("DBM evil &#27;[31mRED", "DBM evil [31mRED"),
            ("&#x9b;2J x", "2J x"),
            ("\u{1b}[31mRED\u{1b}[0m\u{7}", "[31mRED[0m"),
            // Text that reorders or hides itself, and line breaks.
            ("evil\u{202e}gnp.exe", "evilgnp.exe"),
            ("a\u{200b}b\u{feff}", "ab"),
            ("  one\r\n\ttwo\u{2028}three  ", "one two three"),
            // A `<` from an entity is text: nothing reads it again.
            ("&lt;b&gt;bold&lt;/b&gt;", "<b>bold</b>"),
            // Unknown and overlong entities stay as written.
            ("&nope; &#x110000;", "&nope; &#x110000;"),
        ] {
            assert_eq!(
                source_line(input, 200, Entities::Decode),
                expected,
                "{input:?}"
            );
        }
        // Keep: a JSON API's literal `&amp;` is literal, controls still go.
        let keep = |s: &str| source_line(s, 200, Entities::Keep);
        assert_eq!(keep("Tukui &amp; ElvUI\u{1b}"), "Tukui &amp; ElvUI");
        // Cut on a character boundary.
        assert_eq!(source_line("Hello wide world", 5, Entities::Keep), "Hello…");
        assert_eq!(
            source_line("\u{d478}\u{ce74}\u{d478}", 2, Entities::Keep),
            "\u{d478}\u{ce74}…"
        );
    }

    #[test]
    fn bbcode_changelogs_decode_their_entities() {
        assert_eq!(
            bbcode_to_text(
                "[b]Fixed[/b] &quot;bars&quot; &amp; &#27;[31mmore &lt;b&gt;",
                200
            ),
            "Fixed \"bars\" & [31mmore &lt;b>"
        );
        // A decoded known tag is prose: its brackets turn to parentheses.
        assert_eq!(bbcode_to_text("&#91;b&#93;x", 200), "(b)x");
    }
}
