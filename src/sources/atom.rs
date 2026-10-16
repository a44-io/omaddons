//! A minimal Atom decoder for GitHub's `releases.atom` and `commits/<branch>.atom`:
//! each `feed/entry` with its `id`, `title`, `updated`, alternate `link@href` and
//! `content` (a release's notes, escaped HTML in `releases.atom`).
//! A small tag scanner, not an XML parser: it ignores namespace prefixes, unknown
//! elements, comments, processing instructions and CDATA outside the fields it reads,
//! and never fails (a broken feed yields fewer entries).

/// One `<entry>`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AtomEntry {
    pub id: String,
    pub title: String,
    /// RFC 3339, as the feed wrote it.
    pub updated: Option<String>,
    /// The `alternate` link (or the first link without a `rel`).
    pub link: Option<String>,
    /// The `<content>`, unescaped: for `type="html"` the HTML itself.
    pub content: Option<String>,
    /// The `<content>`'s `type` (`html`, `xhtml`, `text`), when it says.
    pub content_type: Option<String>,
}

/// The entries of a feed, in feed order.
pub fn parse(xml: &str) -> Vec<AtomEntry> {
    let mut entries = Vec::new();
    let mut entry: Option<AtomEntry> = None;
    // Open elements inside the current entry, the entry included.
    let mut depth = 0usize;
    // A direct child of the entry whose text is being read: (local name, text start).
    let mut field: Option<(&str, usize)> = None;
    let mut at = 0;
    while let Some(tag) = next_tag(xml, at) {
        at = tag.end;
        let local = tag.qname.rsplit(':').next().unwrap_or(tag.qname);
        if entry.is_none() {
            if local == "entry" && !tag.close && !tag.empty {
                entry = Some(AtomEntry::default());
                depth = 1;
            }
            continue;
        }
        if tag.close {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                entries.extend(entry.take());
                field = None;
            } else if depth == 1
                && let Some((name, start)) = field.take()
                && name == local
                && let (Some(e), Some(raw)) = (entry.as_mut(), xml.get(start..tag.start))
            {
                let text = text(raw);
                match name {
                    "id" => e.id = text,
                    "title" => e.title = text,
                    "content" => e.content = Some(text),
                    _ => e.updated = Some(text),
                }
            }
            continue;
        }
        if depth == 1 {
            match local {
                "id" | "title" | "updated" if !tag.empty => field = Some((local, tag.end)),
                "content" if !tag.empty => {
                    field = Some((local, tag.end));
                    if let Some(e) = entry.as_mut() {
                        e.content_type = attr(tag.attrs, "type");
                    }
                }
                "link" => {
                    let rel = attr(tag.attrs, "rel");
                    if let (Some(e), None | Some("alternate")) = (entry.as_mut(), rel.as_deref())
                        && e.link.is_none()
                    {
                        e.link = attr(tag.attrs, "href");
                    }
                }
                _ => {}
            }
        }
        if !tag.empty {
            depth += 1;
        }
    }
    entries
}

struct Tag<'a> {
    qname: &'a str,
    attrs: &'a str,
    close: bool,
    empty: bool,
    start: usize,
    end: usize,
}

/// The next element tag at or after `from`, skipping comments, CDATA, `<?…?>` and `<!…>`.
fn next_tag(xml: &str, mut from: usize) -> Option<Tag<'_>> {
    loop {
        let start = from + xml.get(from..)?.find('<')?;
        let rest = xml.get(start..)?;
        let skip_to = |marker: &str| rest.find(marker).map(|i| start + i + marker.len());
        if rest.starts_with("<!--") {
            from = skip_to("-->")?;
            continue;
        }
        if rest.starts_with("<![CDATA[") {
            from = skip_to("]]>")?;
            continue;
        }
        if rest.starts_with("<?") || rest.starts_with("<!") {
            from = skip_to(">")?;
            continue;
        }
        // The closing '>' outside quoted attribute values.
        let mut quote = None;
        let close_at = rest
            .char_indices()
            .skip(1)
            .find_map(|(i, c)| match (quote, c) {
                (None, '"' | '\'') => {
                    quote = Some(c);
                    None
                }
                (Some(q), c) if c == q => {
                    quote = None;
                    None
                }
                (None, '>') => Some(i),
                _ => None,
            })?;
        let inner = rest.get(1..close_at)?;
        let close = inner.starts_with('/');
        let inner = inner.trim_start_matches('/');
        let empty = inner.ends_with('/');
        let inner = inner.trim_end_matches('/');
        let name_end = inner.find(char::is_whitespace).unwrap_or(inner.len());
        let (qname, attrs) = inner.split_at(name_end);
        return Some(Tag {
            qname,
            attrs,
            close,
            empty,
            start,
            end: start + close_at + 1,
        });
    }
}

/// An attribute's unescaped value.
fn attr(attrs: &str, name: &str) -> Option<String> {
    let mut rest = attrs;
    loop {
        rest = rest.trim_start();
        let eq = rest.find('=')?;
        let key = rest.get(..eq)?.trim();
        let after = rest.get(eq + 1..)?.trim_start();
        let quote = after.chars().next().filter(|c| *c == '"' || *c == '\'')?;
        let body = after.get(1..)?;
        let end = body.find(quote)?;
        if key == name {
            return Some(unescape(body.get(..end)?));
        }
        rest = body.get(end + 1..)?;
    }
}

/// Element text: CDATA kept verbatim, everything else unescaped; trimmed.
fn text(raw: &str) -> String {
    let mut out = String::new();
    let mut rest = raw;
    while let Some(i) = rest.find("<![CDATA[") {
        out.push_str(&unescape(rest.get(..i).unwrap_or_default()));
        let body = rest.get(i + 9..).unwrap_or_default();
        let end = body.find("]]>").unwrap_or(body.len());
        out.push_str(body.get(..end).unwrap_or_default());
        rest = body.get(end + 3..).unwrap_or_default();
    }
    out.push_str(&unescape(rest));
    out.trim().to_owned()
}

/// Replaces the five XML entities and numeric character references.
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(rest.get(..i).unwrap_or_default());
        let after = rest.get(i + 1..).unwrap_or_default();
        let decoded = after.find(';').filter(|end| *end <= 10).and_then(|end| {
            let name = after.get(..end)?;
            let c = match name {
                "amp" => '&',
                "lt" => '<',
                "gt" => '>',
                "quot" => '"',
                "apos" => '\'',
                _ => {
                    let code = match name.strip_prefix("#x").or_else(|| name.strip_prefix("#X")) {
                        Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                        None => name.strip_prefix('#')?.parse().ok()?,
                    };
                    char::from_u32(code)?
                }
            };
            Some((c, end))
        });
        match decoded {
            Some((c, end)) => {
                out.push(c);
                rest = after.get(end + 1..).unwrap_or_default();
            }
            None => {
                out.push('&');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    /// The shape of GitHub's `releases.atom` (2026-10), with placeholder authors.
    const RELEASES: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom" xmlns:media="http://search.yahoo.com/mrss/" xml:lang="en-US">
  <id>tag:github.com,2008:https://github.com/Questie/Questie/releases</id>
  <link type="text/html" rel="alternate" href="https://github.com/Questie/Questie/releases"/>
  <link type="application/atom+xml" rel="self" href="https://github.com/Questie/Questie/releases.atom"/>
  <title>Release notes from Questie</title>
  <updated>2026-10-12T20:29:01Z</updated>
  <entry>
    <id>tag:github.com,2008:Repository/31859273/v12.1.0</id>
    <updated>2026-10-12T20:29:01Z</updated>
    <link rel="alternate" type="text/html" href="https://github.com/Questie/Questie/releases/tag/v12.1.0"/>
    <title>v12.1.0</title>
    <content type="html">&lt;h2&gt;Questie v12.1.0&lt;/h2&gt; &lt;title&gt;not this&lt;/title&gt;</content>
    <author>
      <name>someone</name>
    </author>
    <media:thumbnail height="30" width="30" url="https://avatars.githubusercontent.com/u/1?s=60&amp;v=4"/>
  </entry>
  <entry>
    <id>tag:github.com,2008:Repository/31859273/bundle/wago/v12.1.0+v1.0.6</id>
    <updated>2026-10-12T20:29:01Z</updated>
    <link rel="alternate" type="text/html" href="https://github.com/Questie/Questie/releases/tag/bundle%2Fwago%2Fv12.1.0%2Bv1.0.6"/>
    <title>bundle/wago/v12.1.0+v1.0.6</title>
    <author><name>someone</name></author>
  </entry>
</feed>
"#;

    #[test]
    fn github_releases_feed() {
        let entries = parse(RELEASES);
        assert_eq!(entries.len(), 2);
        assert_eq!(
            entries[0],
            AtomEntry {
                id: "tag:github.com,2008:Repository/31859273/v12.1.0".into(),
                title: "v12.1.0".into(),
                updated: Some("2026-10-12T20:29:01Z".into()),
                link: Some("https://github.com/Questie/Questie/releases/tag/v12.1.0".into()),
                // Escaped HTML, unescaped once: the HTML, its tags kept.
                content: Some("<h2>Questie v12.1.0</h2> <title>not this</title>".into()),
                content_type: Some("html".into()),
            }
        );
        assert_eq!(entries[1].title, "bundle/wago/v12.1.0+v1.0.6");
        assert_eq!(
            (&entries[1].content, &entries[1].content_type),
            (&None, &None)
        );
        // CDATA content is kept verbatim.
        let cdata = parse(
            "<feed><entry><content type=\"html\"><![CDATA[<p>a &amp; b</p>]]></content></entry></feed>",
        );
        assert_eq!(cdata[0].content.as_deref(), Some("<p>a &amp; b</p>"));
        assert!(
            entries[1]
                .link
                .as_deref()
                .unwrap()
                .ends_with("bundle%2Fwago%2Fv12.1.0%2Bv1.0.6")
        );
    }

    #[test]
    fn prefixes_cdata_comments_and_nesting() {
        let xml = r#"<?xml version="1.0"?>
<!DOCTYPE feed>
<a:feed xmlns:a="http://www.w3.org/2005/Atom">
  <!-- <a:entry><a:id>commented out</a:id></a:entry> -->
  <a:entry>
    <a:link rel="self" href="https://x.org/self"/>
    <a:link rel='alternate' type="text/html" href='https://x.org/a?b=1&amp;c=2'/>
    <a:title type="html"><![CDATA[Fix <b>&amp;</b> things]]></a:title>
    <a:id> urn:1 </a:id>
    <a:source><a:id>urn:source</a:id><a:title>not the title</a:title></a:source>
    <a:updated>2026-01-01T00:00:00Z</a:updated>
  </a:entry>
  <a:entry><a:title>caf&#233; &#x2713; &bogus; &amp</a:title></a:entry>
</a:feed>"#;
        let entries = parse(xml);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].id, "urn:1");
        assert_eq!(entries[0].title, "Fix <b>&amp;</b> things");
        assert_eq!(entries[0].link.as_deref(), Some("https://x.org/a?b=1&c=2"));
        assert_eq!(entries[0].updated.as_deref(), Some("2026-01-01T00:00:00Z"));
        assert_eq!(entries[1].title, "café ✓ &bogus; &amp");
        assert_eq!(
            (entries[1].link.as_deref(), entries[1].updated.as_deref()),
            (None, None)
        );
    }

    #[test]
    fn broken_feeds_yield_what_parsed() {
        assert!(parse("").is_empty());
        assert!(parse("not xml at all").is_empty());
        assert!(
            parse("<feed><entry><id>x</id>").is_empty(),
            "an unclosed entry is dropped"
        );
        assert_eq!(
            parse("<feed><entry><id>x</id></entry><entry><id>y").len(),
            1
        );
        assert_eq!(
            parse("<entry><link href=\"a>b\"/></entry>")[0]
                .link
                .as_deref(),
            Some("a>b")
        );
    }

    proptest! {
        #[test]
        fn never_panics(xml in "[<>/!?\\[\\]a-z:=\"' &;#x0-9-]{0,200}") {
            let _ = parse(&xml);
        }
    }
}
