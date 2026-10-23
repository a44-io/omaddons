//! The frames of a Lua error (M3-D18 rules 2 to 4): the message's location, then each
//! stack line, each read for the path it names in any BugGrabber era. Borrowed slices
//! only; a chunk frame (custom code) is marked, never read for content.

/// One frame of an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame<'a> {
    /// The frame's text, trimmed.
    pub raw: &'a str,
    /// The file it names, `...` included when the client truncated it; r152's library
    /// form is `Major-Minor (Folder)`.
    pub path: Option<&'a str>,
    pub line: Option<u32>,
    pub kind: FrameKind,
}

/// What a frame is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameKind {
    /// It names a file.
    Path,
    /// Code from a string (`[string "…"]`, `[*…xml:…]`, `[<code>]`): custom code,
    /// shown as such and never by content.
    Chunk,
    /// `[C]`.
    CCode,
    /// `[tail call]` or `(tail call)`.
    TailCall,
    Other,
}

/// The frames of an error, in order: the message's location (its first line up to the
/// first `: ` after a `:\d+`), then each stack line. A `LUA_WARNING: ` prefix is
/// stripped, and a stack embedded after `|cffffd200Stack:|r` replaces a missing one.
/// With `legacy` (r152), the stack lines are the message's lines after the first,
/// before `"\nLocals:|r\n"`.
pub fn frames<'a>(message: &'a str, stack: Option<&'a str>, legacy: bool) -> Vec<Frame<'a>> {
    let message = strip_prefix_ci(message, "LUA_WARNING: ").unwrap_or(message);
    let (message, embedded) = match message.split_once("|cffffd200Stack:|r") {
        Some((message, stack)) => (message, Some(stack)),
        None => (message, None),
    };
    let first = message.lines().next().unwrap_or_default();
    let mut out: Vec<Frame<'a>> = location(first)
        .map(|location| frame(location, legacy))
        .into_iter()
        .collect();
    let mut add = |text: &'a str| {
        for line in text.lines() {
            if !line.trim().is_empty() {
                out.push(frame(line, legacy));
            }
        }
    };
    if legacy {
        let body = message
            .split_once("\nLocals:|r\n")
            .map_or(message, |(m, _)| m);
        if let Some((_, rest)) = body.split_once('\n') {
            add(rest);
        }
    }
    // The entry's own stack; an embedded one only in place of a missing (or empty) one.
    if let Some(stack) = stack.filter(|s| !s.trim().is_empty()).or(embedded) {
        add(stack);
    }
    out
}

/// A message's first line up to its location's `:<line>:`, when it has one.
pub(crate) fn location(first: &str) -> Option<&str> {
    let bytes = first.as_bytes();
    let mut from = 0;
    while let Some(at) = first[from..].find(':') {
        let colon = from + at;
        let digits = bytes[colon + 1..]
            .iter()
            .take_while(|b| b.is_ascii_digit())
            .count();
        let end = colon + 1 + digits;
        if digits > 0 && bytes.get(end) == Some(&b':') && bytes.get(end + 1) == Some(&b' ') {
            return Some(&first[..=end]);
        }
        from = colon + 1;
    }
    None
}

/// One frame, read (M3-D18 rule 4).
pub fn frame(raw: &str, legacy: bool) -> Frame<'_> {
    let raw = raw.trim_matches(|c: char| c == '\r' || c == ' ' || c == '\t');
    let of = |kind| Frame {
        raw,
        path: None,
        line: None,
        kind,
    };
    let dots = raw.bytes().take(3).take_while(|b| *b == b'.').count();
    let rest = &raw[dots..];
    if let Some(bracketed) = rest.strip_prefix('[') {
        // Era B: `[string "@Interface/AddOns/X/f.lua"]:12:`.
        if let Some(inner) = rest.strip_prefix("[string \"@") {
            return inner
                .split_once("\"]")
                .and_then(|(path, after)| path_frame(raw, path, after))
                .unwrap_or(of(FrameKind::Chunk));
        }
        // Era B names C code and tail calls as chunks of their own: `[string "=[C]"]`,
        // `[string "=(tail call)"]`.
        if rest.starts_with("[string \"=[C]\"]") {
            return of(FrameKind::CCode);
        }
        if rest.starts_with("[string \"=(tail call)\"]") {
            return of(FrameKind::TailCall);
        }
        if rest.starts_with("[string \"") || rest.starts_with("[*") {
            return of(FrameKind::Chunk);
        }
        if rest.starts_with("[C]") {
            return of(FrameKind::CCode);
        }
        if rest.starts_with("[tail call]") {
            return of(FrameKind::TailCall);
        }
        // Era C: `[Interface/AddOns/X/f.lua]:12:`.
        return bracketed
            .split_once(']')
            // `[<code>]` is code, not a path.
            .filter(|(path, _)| !path.is_empty() && !path.contains('"') && !path.starts_with('<'))
            .and_then(|(path, after)| path_frame(raw, path, after))
            .unwrap_or(of(FrameKind::Chunk));
    }
    if raw.starts_with("(tail call)") {
        return of(FrameKind::TailCall);
    }
    // Era A, and messages: `Interface\AddOns\X\f.lua:12:` (paths may hold spaces; a
    // truncated era C location keeps its `]`). r152's library form: `Major-Minor
    // (Folder):12:`.
    let Some(colon) = raw.find(':') else {
        return of(FrameKind::Other);
    };
    let (path, after) = raw.split_at(colon);
    let first = rest.bytes().next();
    let file = path.strip_suffix(']').unwrap_or(path);
    let named = has_extension(file, ".lua") || has_extension(file, ".xml");
    let library = legacy && path.ends_with(')') && path.contains(" (");
    if matches!(first, Some(b'[' | b'<' | b':') | None) || !(named || library) {
        return of(FrameKind::Other);
    }
    match line_number(after) {
        Some(line) => Frame {
            raw,
            path: Some(if named { file } else { path }),
            line: Some(line),
            kind: FrameKind::Path,
        },
        None => of(FrameKind::Other),
    }
}

/// A path frame when `after` (what follows the path) reads `:<line>:`.
fn path_frame<'a>(raw: &'a str, path: &'a str, after: &str) -> Option<Frame<'a>> {
    Some(Frame {
        raw,
        path: Some(path),
        line: Some(line_number(after)?),
        kind: FrameKind::Path,
    })
}

/// The line of a `:<digits>:` prefix.
fn line_number(after: &str) -> Option<u32> {
    let rest = after.strip_prefix(':')?;
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 || rest.as_bytes().get(digits) != Some(&b':') {
        return None;
    }
    rest[..digits].parse().ok()
}

fn has_extension(path: &str, extension: &str) -> bool {
    path.len() >= extension.len()
        && path.is_char_boundary(path.len() - extension.len())
        && path[path.len() - extension.len()..].eq_ignore_ascii_case(extension)
}

/// `text` without `prefix`, compared without ASCII case.
pub(crate) fn strip_prefix_ci<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    let head = text.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &text[prefix.len()..])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(raw: &str, legacy: bool) -> (Option<&str>, Option<u32>, FrameKind) {
        let f = frame(raw, legacy);
        (f.path, f.line, f.kind)
    }

    #[test]
    fn every_era_names_its_path() {
        use FrameKind::*;
        for (raw, expected) in [
            (
                "Interface\\AddOns\\Foo\\core.lua:12: in function `x'",
                (Some("Interface\\AddOns\\Foo\\core.lua"), Some(12), Path),
            ),
            (
                "[string \"@Interface/AddOns/Foo/core.lua\"]:12: in function 'x'",
                (Some("Interface/AddOns/Foo/core.lua"), Some(12), Path),
            ),
            (
                "[Interface/AddOns/Foo/core.lua]:12: in function 'x'",
                (Some("Interface/AddOns/Foo/core.lua"), Some(12), Path),
            ),
            (
                "...[Interface/AddOns/Foo/core.lua]:7:",
                (Some("Interface/AddOns/Foo/core.lua"), Some(7), Path),
            ),
            (
                "..._EncounterWarnings/EncounterWarningsViewElements.lua:40: x",
                (
                    Some("..._EncounterWarnings/EncounterWarningsViewElements.lua"),
                    Some(40),
                    Path,
                ),
            ),
            (
                "...ns/Foo/Modules/Long Name.LUA]:3:",
                (Some("...ns/Foo/Modules/Long Name.LUA"), Some(3), Path),
            ),
            (
                "  Interface\\AddOns\\Foo\\ui.xml:5: in main chunk\r",
                (Some("Interface\\AddOns\\Foo\\ui.xml"), Some(5), Path),
            ),
        ] {
            assert_eq!(path(raw, false), expected, "{raw}");
        }
    }

    #[test]
    fn r152_forms_are_paths() {
        assert_eq!(
            path("Recount-3.3.5-r1\\Recount.lua:123: in function `x'", true),
            (
                Some("Recount-3.3.5-r1\\Recount.lua"),
                Some(123),
                FrameKind::Path
            )
        );
        assert_eq!(
            path(
                "CallbackHandler-1.0-6 (Details):146: in function `Fire'",
                true
            ),
            (
                Some("CallbackHandler-1.0-6 (Details)"),
                Some(146),
                FrameKind::Path
            )
        );
        assert_eq!(
            path("CallbackHandler-1.0-6 (Details):146:", false).2,
            FrameKind::Other,
            "only r152 writes the library form"
        );
    }

    #[test]
    fn code_and_c_frames_are_marked_never_read() {
        use FrameKind::*;
        for (raw, kind) in [
            ("[C]: in function 'xpcall'", CCode),
            ("[C]: ?", CCode),
            ("[tail call]: ?", TailCall),
            ("(tail call): ?", TailCall),
            ("[string \"return function() FakeChar end\"]:2: in x", Chunk),
            ("[string \"=(tail call)\"]: ?", TailCall),
            ("[string \"=[C]\"]: in function 'xpcall'", CCode),
            ("[string \"=[C]x\"]:1: in x", Chunk),
            ("[*:Foo.xml:3]: in x", Chunk),
            ("[<code>]:1:", Chunk),
            ("in function <Interface/AddOns/Foo/x.lua:3>", Other),
            ("<Interface/AddOns/Foo/x.lua>:3:", Other),
            ("Foo/core:12:", Other),
            ("Foo/core.lua:x:", Other),
            ("", Other),
        ] {
            let f = frame(raw, false);
            assert_eq!((f.kind, f.path), (kind, None), "{raw}");
        }
    }

    #[test]
    fn the_location_comes_first_then_the_stack() {
        let found = frames(
            "[Interface/AddOns/Foo/core.lua]:12: attempt to index a nil value (field 'x:1: y')\nsecond line",
            Some(
                "[Interface/AddOns/Foo/core.lua]:12: in function 'A'\n[C]: in function 'xpcall'\n\n",
            ),
            false,
        );
        let kinds: Vec<FrameKind> = found.iter().map(|f| f.kind).collect();
        assert_eq!(kinds, [FrameKind::Path, FrameKind::Path, FrameKind::CCode]);
        assert_eq!(found[0].raw, "[Interface/AddOns/Foo/core.lua]:12:");
        assert!(frames("[ADDON_ACTION_FORBIDDEN] AddOn 'Foo' tried", None, false).is_empty());
    }

    #[test]
    fn a_warning_brings_its_stack_in_the_message() {
        let found = frames(
            "lua_warning: [Interface/AddOns/Baz/Baz.xml]:5: odd\n|cffffd200Stack:|r\n[Interface/AddOns/Baz/Baz.lua]:20: in main chunk",
            None,
            false,
        );
        let paths: Vec<Option<&str>> = found.iter().map(|f| f.path).collect();
        assert_eq!(
            paths,
            [
                Some("Interface/AddOns/Baz/Baz.xml"),
                Some("Interface/AddOns/Baz/Baz.lua")
            ]
        );
    }

    #[test]
    fn r152_frames_are_the_message_lines_before_the_locals() {
        let found = frames(
            "Foo-1.2\\core.lua:12: attempt to index\nFoo-1.2\\core.lua:12: in function `Refresh'\nAceEvent-3 (Foo):120: in function `?'\n[C]: in function `pcall'\nLocals:|r\nself = 1",
            None,
            true,
        );
        let kinds: Vec<FrameKind> = found.iter().map(|f| f.kind).collect();
        assert_eq!(
            kinds,
            [
                FrameKind::Path,
                FrameKind::Path,
                FrameKind::Path,
                FrameKind::CCode
            ]
        );
    }
}
