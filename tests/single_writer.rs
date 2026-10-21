//! The single-writer rule (D26) as a test: outside `src/fsx/`, production code in `src/`
//! may read the filesystem but never write, rename, remove, create or chmod anything,
//! only an explicit allowlist of modules may call `prune()`, and only another may start
//! a process.
//!
//! The scanner is text matching over a small lexer that drops comments and blanks the
//! contents of literals, carrying its state across lines. A false positive is fixed by
//! moving the code into fsx, never by weakening a pattern; an exception needs a
//! vision-bead comment. Test code is not production code (DEC-8): every item a `cfg`
//! that requires `test` marks is skipped, at any depth, and files marked `#![cfg(test)]`
//! are skipped whole.

#[cfg(test)]
mod scanner {
    use std::path::{Path, PathBuf};

    /// Write-side calls that may appear only in `src/fsx/`.
    const PATTERNS: &[&str] = &[
        "fs::write(",
        "fs::rename(",
        "fs::remove_file(",
        "fs::remove_dir(",
        "fs::remove_dir_all(",
        "fs::create_dir(",
        "fs::create_dir_all(",
        "fs::copy(",
        "fs::hard_link(",
        "fs::set_permissions(",
        "fs::soft_link(",
        "os::unix::fs::symlink(",
        "os::unix::fs::chown(",
        "os::unix::fs::lchown(",
        "os::unix::fs::fchown(",
        "File::create(",
        "File::create_new(",
        "File::options(",
        "OpenOptions",
        "DirBuilder",
        ".set_len(",
        ".set_permissions(",
        ".set_modified(",
        ".set_times(",
        "tempfile::",
        "NamedTempFile",
        "TempPath",
        ".persist(",
        ".persist_noclobber(",
        ".extract(",
        "reflink_copy::",
        // A write-mode open is a write.
        "OFlags::CREATE",
        "OFlags::TRUNC",
        "OFlags::WRONLY",
        "OFlags::RDWR",
        "OFlags::APPEND",
        "OFlags::TMPFILE",
    ];

    /// rustix calls that write: flagged after `rustix::fs::` or an import of the
    /// `rustix::fs` module, and when imported themselves.
    const RUSTIX_WRITES: &[&str] = &[
        "rename",
        "renameat",
        "renameat_with",
        "unlink",
        "unlinkat",
        "mkdir",
        "mkdirat",
        "rmdir",
        "chmod",
        "chmodat",
        "fchmod",
        "chown",
        "chownat",
        "fchown",
        "symlink",
        "symlinkat",
        "link",
        "linkat",
        "ftruncate",
        "fallocate",
        "copy_file_range",
        "ioctl_ficlone",
        "utimensat",
        "futimens",
        "mknodat",
        "setxattr",
        "fsetxattr",
        "lsetxattr",
        "removexattr",
        "fremovexattr",
        "lremovexattr",
    ];

    /// Names that may not be imported from `std::fs` outside fsx.
    const FS_IMPORTS: &[&str] = &[
        "write",
        "rename",
        "remove_file",
        "remove_dir",
        "remove_dir_all",
        "create_dir",
        "create_dir_all",
        "copy",
        "hard_link",
        "set_permissions",
        "soft_link",
        "File",
        "OpenOptions",
        "DirBuilder",
    ];

    /// Names that may not be imported from `std::os::unix::fs` outside fsx.
    const UNIX_FS_IMPORTS: &[&str] = &["symlink", "chown", "lchown", "fchown"];

    /// What starts a process, matched as whole identifiers.
    const SPAWNS: &[&str] = &["Command::new(", "process::Command"];

    /// Modules allowed to start a process, each with its reason (D26: a process that
    /// writes is a writer).
    pub const SPAWNERS: &[(&str, &str)] = &[
        (
            "src/sources/git_hosts.rs",
            "git checkouts in <cache>/omaddons/tmp/<tx>/git, removed with the tx's temp dir",
        ),
        (
            "src/state/config.rs",
            "credential_command: reads its stdout, writes nothing",
        ),
    ];

    /// Words that start an item: a top-level `,` ends no item that starts with one (the
    /// commas of `impl<A, B>` or `fn f<A, B>`), but ends a variant, a field or an arm.
    const ITEM_WORDS: &[&str] = &[
        "pub",
        "fn",
        "mod",
        "use",
        "impl",
        "struct",
        "enum",
        "union",
        "trait",
        "type",
        "const",
        "static",
        "extern",
        "unsafe",
        "async",
        "macro_rules",
        "let",
    ];

    fn is_ident(c: char) -> bool {
        c.is_alphanumeric() || c == '_'
    }

    enum State {
        Code,
        Str,
        /// A raw string with this many `#`.
        Raw(usize),
        /// A block comment this deeply nested.
        Block(usize),
    }

    /// `text`'s code, line by line: comments dropped and the contents of string, raw
    /// string and char literals blanked (their quotes kept), with the lexer's state
    /// carried across lines.
    pub fn code_lines(text: &str) -> Vec<String> {
        let chars: Vec<char> = text.chars().collect();
        let at = |i: usize| chars.get(i).copied();
        let mut out = String::with_capacity(text.len());
        let mut state = State::Code;
        let mut i = 0;
        while let Some(c) = at(i) {
            match state {
                State::Code => {
                    if c == '/' && at(i + 1) == Some('/') {
                        while at(i).is_some_and(|c| c != '\n') {
                            i += 1;
                        }
                        continue;
                    }
                    if c == '/' && at(i + 1) == Some('*') {
                        state = State::Block(1);
                        i += 2;
                        continue;
                    }
                    if c == '"' {
                        out.push('"');
                        state = State::Str;
                        i += 1;
                        continue;
                    }
                    // `r"`, `r#"`, `br"`, `cr"`: not the `r` of a longer identifier.
                    let word: String = out.chars().rev().take_while(|c| is_ident(*c)).collect();
                    if c == 'r' && matches!(word.as_str(), "" | "b" | "c") {
                        let hashes = (i + 1..).take_while(|&j| at(j) == Some('#')).count();
                        if at(i + 1 + hashes) == Some('"') {
                            out.push_str("r\"");
                            state = State::Raw(hashes);
                            i += 2 + hashes;
                            continue;
                        }
                    }
                    if c == '\''
                        && let Some(end) = char_literal_end(&chars, i)
                    {
                        out.push_str("''");
                        i = end + 1;
                        continue;
                    }
                    out.push(c);
                    i += 1;
                }
                State::Str => match c {
                    '\\' => {
                        if at(i + 1) == Some('\n') {
                            out.push('\n');
                        }
                        i += 2;
                    }
                    '"' => {
                        out.push('"');
                        state = State::Code;
                        i += 1;
                    }
                    _ => {
                        if c == '\n' {
                            out.push('\n');
                        }
                        i += 1;
                    }
                },
                State::Raw(hashes) => {
                    if c == '"' && (1..=hashes).all(|k| at(i + k) == Some('#')) {
                        out.push('"');
                        state = State::Code;
                        i += 1 + hashes;
                    } else {
                        if c == '\n' {
                            out.push('\n');
                        }
                        i += 1;
                    }
                }
                State::Block(depth) => {
                    if c == '/' && at(i + 1) == Some('*') {
                        state = State::Block(depth + 1);
                        i += 2;
                    } else if c == '*' && at(i + 1) == Some('/') {
                        state = if depth == 1 {
                            State::Code
                        } else {
                            State::Block(depth - 1)
                        };
                        i += 2;
                    } else {
                        if c == '\n' {
                            out.push('\n');
                        }
                        i += 1;
                    }
                }
            }
        }
        out.split('\n')
            .map(|line| line.trim_end_matches('\r').to_owned())
            .collect()
    }

    /// The index of the closing quote when `chars[i]` opens a char literal (`'x'`,
    /// `'\n'`, `'\u{1F600}'`); `None` for a lifetime or a label (`'a`).
    fn char_literal_end(chars: &[char], i: usize) -> Option<usize> {
        match chars.get(i + 1)? {
            '\\' => (i + 3..chars.len())
                .take_while(|&j| chars[j] != '\n')
                .find(|&j| chars[j] == '\''),
            '\n' => None,
            _ => (chars.get(i + 2) == Some(&'\'')).then_some(i + 2),
        }
    }

    /// The index of the bracket closing the one at `open` (any of `()[]{}`).
    fn matching(code: &[char], open: usize) -> Option<usize> {
        let mut depth = 0usize;
        for (j, c) in code.iter().enumerate().skip(open) {
            match c {
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => {
                    depth = depth.checked_sub(1)?;
                    if depth == 0 {
                        return Some(j);
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// The index just past the item starting at `from` (after its `cfg`): its further
    /// attributes, then up to a `;` outside brackets or through its first `{ … }`; a `,`
    /// ends it too unless it starts with an [`ITEM_WORDS`] word. It never runs past the
    /// bracket that encloses it.
    fn item_end(code: &[char], from: usize) -> usize {
        let mut i = from;
        let mut first_word: Option<String> = None;
        while i < code.len() {
            let c = code[i];
            if first_word.is_none() && is_ident(c) {
                first_word = Some(code[i..].iter().take_while(|c| is_ident(**c)).collect());
            }
            match c {
                ';' => return i + 1,
                ',' if !first_word
                    .as_deref()
                    .is_some_and(|w| ITEM_WORDS.contains(&w)) =>
                {
                    return i + 1;
                }
                '{' => return matching(code, i).map_or(code.len(), |close| close + 1),
                // An attribute (`#[…]`), arguments, an array type.
                '(' | '[' => i = matching(code, i).map_or(code.len(), |close| close + 1),
                ')' | ']' | '}' => return i,
                _ => i += 1,
            }
        }
        code.len()
    }

    /// Whether an attribute's text (inside `#[…]`) is a `cfg` that holds only in tests:
    /// `test`, or an `all(…)` with such a predicate. `not(test)` and `any(test, …)` do not.
    fn requires_test(attr: &str) -> bool {
        fn holds_only_in_tests(predicate: &str) -> bool {
            predicate == "test"
                || predicate
                    .strip_prefix("all(")
                    .and_then(|rest| rest.strip_suffix(')'))
                    .is_some_and(|args| top_level(args).into_iter().any(holds_only_in_tests))
        }
        let attr: String = attr.chars().filter(|c| !c.is_whitespace()).collect();
        attr.strip_prefix("cfg(")
            .and_then(|rest| rest.strip_suffix(')'))
            .is_some_and(holds_only_in_tests)
    }

    /// `args` split at the commas outside parentheses.
    fn top_level(args: &str) -> Vec<&str> {
        let mut parts = Vec::new();
        let (mut depth, mut start) = (0usize, 0);
        for (j, c) in args.char_indices() {
            match c {
                '(' => depth += 1,
                ')' => depth = depth.saturating_sub(1),
                ',' if depth == 0 => {
                    parts.push(&args[start..j]);
                    start = j + 1;
                }
                _ => {}
            }
        }
        parts.push(&args[start..]);
        parts
    }

    /// A file's production code as one text, lines kept: [`code_lines`] with every
    /// test-only item blanked. `None` for a file marked `#![cfg(test)]`.
    fn production_code(text: &str) -> Option<String> {
        let lines = code_lines(text);
        if lines.iter().any(|line| line.trim() == "#![cfg(test)]") {
            return None;
        }
        let mut code: Vec<char> = lines.join("\n").chars().collect();
        let mut i = 0;
        while i < code.len() {
            if code[i] == '#' && code.get(i + 1) == Some(&'[') {
                let Some(close) = matching(&code, i + 1) else {
                    break;
                };
                let attr: String = code[i + 2..close].iter().collect();
                if requires_test(&attr) {
                    let end = item_end(&code, close + 1);
                    for c in &mut code[i..end] {
                        if *c != '\n' {
                            *c = ' ';
                        }
                    }
                    i = end;
                } else {
                    i = close + 1;
                }
                continue;
            }
            i += 1;
        }
        Some(code.into_iter().collect())
    }

    /// A file's production lines that keep any code, as (1-based line, code).
    pub fn production_lines(text: &str) -> Vec<(usize, String)> {
        production_code(text).map_or_else(Vec::new, |code| {
            code.split('\n')
                .enumerate()
                .filter(|(_, line)| !line.trim().is_empty())
                .map(|(index, line)| (index + 1, line.to_owned()))
                .collect()
        })
    }

    /// An imported full path and its `as` alias.
    type Import = (String, Option<String>);

    /// Every `use` statement of `code` as (1-based line, what it imports).
    fn imports(code: &str) -> Vec<(usize, Vec<Import>)> {
        let mut found = Vec::new();
        let mut from = 0;
        while let Some(at) = code.get(from..).and_then(|rest| rest.find("use")) {
            let at = from + at;
            from = at + 3;
            let before = code[..at].chars().next_back();
            let after = code[at + 3..].chars().next();
            if before.is_some_and(is_ident) || !after.is_some_and(char::is_whitespace) {
                continue;
            }
            let Some(end) = code[at..].find(';').map(|k| at + k) else {
                break;
            };
            let parts = use_parts(&code[at + 3..end]);
            let mut paths = Vec::new();
            let mut pos = usize::from(is(&parts, 0, "::"));
            use_tree(&parts, &mut pos, "", &mut paths);
            found.push((code[..at].matches('\n').count() + 1, paths));
            from = end;
        }
        found
    }

    /// A use tree's parts: identifiers, `*`, `::`, `{`, `}` and `,`.
    fn use_parts(tree: &str) -> Vec<String> {
        let mut parts = Vec::new();
        let mut chars = tree.chars().peekable();
        while let Some(c) = chars.next() {
            if is_ident(c) {
                let mut word = c.to_string();
                while let Some(&next) = chars.peek().filter(|n| is_ident(**n)) {
                    word.push(next);
                    chars.next();
                }
                parts.push(word);
            } else if c == ':' && chars.peek() == Some(&':') {
                chars.next();
                parts.push("::".to_owned());
            } else if matches!(c, '*' | '{' | '}' | ',') {
                parts.push(c.to_string());
            }
        }
        parts
    }

    /// Whether the part at `pos` is `what`.
    fn is(parts: &[String], pos: usize, what: &str) -> bool {
        parts.get(pos).is_some_and(|part| part.as_str().eq(what))
    }

    /// One use tree under `prefix`: a path, then a `{ … }` group or an `as` alias.
    fn use_tree(parts: &[String], pos: &mut usize, prefix: &str, out: &mut Vec<Import>) {
        let mut path = prefix.to_owned();
        while let Some(part) = parts.get(*pos) {
            if is(parts, *pos, "{") {
                *pos += 1;
                while *pos < parts.len() && !is(parts, *pos, "}") {
                    let before = *pos;
                    use_tree(parts, pos, &path, out);
                    // Past the `,`, or past a part no tree starts with.
                    if *pos == before || is(parts, *pos, ",") {
                        *pos += 1;
                    }
                }
                *pos += 1;
                return;
            }
            if !is(parts, *pos, "self") {
                path = if path.is_empty() {
                    part.clone()
                } else {
                    format!("{path}::{part}")
                };
            }
            *pos += 1;
            if is(parts, *pos, "::") {
                *pos += 1;
                continue;
            }
            let alias = if is(parts, *pos, "as") {
                *pos += 2;
                parts.get(*pos - 1).cloned()
            } else {
                None
            };
            out.push((path, alias));
            return;
        }
    }

    /// Whether `code` holds `pattern` with no identifier character right after it.
    fn contains_word(code: &str, pattern: &str) -> bool {
        code.match_indices(pattern).any(|(at, _)| {
            !code[at + pattern.len()..]
                .chars()
                .next()
                .is_some_and(is_ident)
        })
    }

    /// The identifiers right after each `<module>::` in `code`, where `module` starts
    /// after a non-identifier (and, for a one-word alias, a non-`:`).
    fn calls_into<'a>(code: &'a str, module: &str) -> Vec<&'a str> {
        let pattern = format!("{module}::");
        code.match_indices(&pattern)
            .filter(|(at, _)| {
                code[..*at]
                    .chars()
                    .next_back()
                    .is_none_or(|c| !is_ident(c) && (module.contains("::") || c != ':'))
            })
            .map(|(at, _)| {
                let rest = &code[at + pattern.len()..];
                &rest[..rest.find(|c| !is_ident(c)).unwrap_or(rest.len())]
            })
            .collect()
    }

    /// Every violation in one file, as `label:line: what`.
    pub fn scan_source(label: &str, text: &str) -> Vec<String> {
        let Some(code) = production_code(text) else {
            return Vec::new();
        };
        let mut found = Vec::new();
        let at = |line: usize, what: &str| format!("{label}:{line}: {what}");
        let spawner = SPAWNERS.iter().any(|(file, _)| *file == label);
        let mut rustix_modules = vec!["rustix::fs".to_owned()];
        for (line, paths) in imports(&code) {
            for (path, alias) in paths {
                let (module, name) = path.rsplit_once("::").unwrap_or(("", &path));
                let banned = match module {
                    "std::fs" => name == "*" || FS_IMPORTS.contains(&name),
                    "std::os::unix::fs" => name == "*" || UNIX_FS_IMPORTS.contains(&name),
                    "rustix::fs" => name == "*" || RUSTIX_WRITES.contains(&name),
                    "std::process" => !spawner && name == "Command",
                    _ => false,
                };
                if banned {
                    found.push(at(line, &format!("use {path}")));
                }
                match (path.as_str(), alias) {
                    ("std::fs", Some(_)) => found.push(at(line, "use std::fs as")),
                    ("rustix::fs", alias) => {
                        rustix_modules.push(alias.unwrap_or_else(|| "fs".to_owned()));
                    }
                    _ => {}
                }
            }
        }
        for (index, line) in code.split('\n').enumerate() {
            let line_no = index + 1;
            for pattern in PATTERNS {
                if line.contains(pattern) {
                    found.push(at(line_no, pattern));
                }
            }
            for module in &rustix_modules {
                for call in calls_into(line, module) {
                    if RUSTIX_WRITES.contains(&call) {
                        found.push(at(line_no, &format!("rustix::fs::{call}")));
                    }
                }
            }
            if !spawner && SPAWNS.iter().any(|spawn| contains_word(line, spawn)) {
                found.push(at(line_no, "process spawn"));
            }
        }
        found
    }

    /// Every `*.rs` file under `dir`.
    pub fn rust_files(dir: &Path) -> Vec<PathBuf> {
        let mut files = Vec::new();
        let mut pending = vec![dir.to_path_buf()];
        while let Some(dir) = pending.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    pending.push(path);
                } else if matches!(path.extension().and_then(|e| e.to_str()), Some("rs")) {
                    files.push(path);
                }
            }
        }
        files.sort();
        files
    }

    pub fn root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    }

    pub fn label(path: &Path) -> String {
        path.strip_prefix(root()).unwrap().display().to_string()
    }
}

use scanner::{SPAWNERS, code_lines, label, production_lines, root, rust_files, scan_source};

#[test]
fn the_scanner_flags_writes_and_ignores_reads_comments_and_strings() {
    let flagged = scan_source(
        "src/app/x.rs",
        "fn f() {\n    std::fs::write(\"x\", b\"y\").unwrap();\n}\n",
    );
    assert_eq!(flagged, ["src/app/x.rs:2: fs::write("]);
    let write = "std::fs::write(\"a\", \"b\").unwrap();";
    for (source, expected) in [
        (
            "let s = fs::read_to_string(p)?; let m = fs::metadata(p)?; File::open(p)?;".to_owned(),
            0,
        ),
        ("// fs::remove_file(p) in a comment".to_owned(), 0),
        ("let doc = \"call fs::remove_dir_all( here\";".to_owned(), 0),
        ("let x = 1; // then fs::rename(a, b)".to_owned(), 0),
        ("use std::fs as f;".to_owned(), 1),
        ("use std::{fs as f, io};".to_owned(), 1),
        ("use std::fs::{read, write};".to_owned(), 1),
        ("use std::fs::{\n    read_dir,\n    OpenOptions,\n};".to_owned(), 2),
        ("use std::fs::File;".to_owned(), 1),
        ("use std::fs::*;".to_owned(), 1),
        ("rustix::fs::renameat_with(a, b, c, d, e)".to_owned(), 1),
        ("let t = tempfile::tempdir()?;".to_owned(), 1),
        ("file.set_len(0)?;".to_owned(), 1),
        // Production code after a test item, however it is declared.
        (format!("#[cfg(test)]\nmod tests;\nfn f() {{ {write} }}"), 1),
        (format!("#[cfg(test)]\nfn helper() {{}}\nfn f() {{ {write} }}"), 1),
        (
            format!("    #[cfg(test)]\n    fn helper() {{ {write} }}\n    fn f() {{ {write} }}"),
            1,
        ),
        (
            format!(
                "#[cfg(test)]\n#[expect(dead_code, reason = \"]\")]\nfn t() {{ {write} }}\nfn f() {{ {write} }}"
            ),
            1,
        ),
        (format!("#[cfg(not(test))]\nfn f() {{ {write} }}"), 1),
        (format!("#[cfg(any(test, unix))]\nfn f() {{ {write} }}"), 1),
        (
            format!("match p {{\n    #[cfg(test)]\n    P::T => 1,\n    P::U => {{ {write} 2 }}\n}}"),
            1,
        ),
        // Write APIs.
        (
            "std::fs::File::options().write(true).create(true).open(\"p\").unwrap();".to_owned(),
            1,
        ),
        ("std::fs::DirBuilder::new().create(\"p\").unwrap();".to_owned(), 1),
        ("file.set_permissions(perms)?;".to_owned(), 1),
        ("file.set_modified(t)?;".to_owned(), 1),
        ("file.set_times(t)?;".to_owned(), 1),
        ("std::os::unix::fs::chown(p, None, None)?;".to_owned(), 1),
        ("std::os::unix::fs::lchown(p, None, None)?;".to_owned(), 1),
        ("t.persist_noclobber(p)?;".to_owned(), 1),
        ("let t = NamedTempFile::new_in(d)?;".to_owned(), 1),
        ("archive.extract(dir)?;".to_owned(), 1),
        ("use std::os::unix::fs::{symlink, PermissionsExt};".to_owned(), 1),
        // rustix.
        (
            "use rustix::fs::{\n    renameat,\n    CWD,\n};\nfn f() { renameat(CWD, \"a\", CWD, \"b\").unwrap(); }"
                .to_owned(),
            1,
        ),
        (
            "rustix::fs::open(p, OFlags::WRONLY | OFlags::CREATE, Mode::RUSR)?;".to_owned(),
            2,
        ),
        ("rustix::fs::unlink(p)?;".to_owned(), 1),
        ("rustix::fs::mkdir(p, Mode::RWXU)?;".to_owned(), 1),
        ("rustix::fs::chmod(p, Mode::RWXU)?;".to_owned(), 1),
        ("rustix::fs::ftruncate(fd, 0)?;".to_owned(), 1),
        (
            "use rustix::fs;\nfn f() { fs::unlinkat(CWD, p, AtFlags::empty()).unwrap(); }"
                .to_owned(),
            1,
        ),
        (
            "use rustix::{fs as rfs, io};\nfn f() { rfs::rmdir(p).unwrap(); }".to_owned(),
            1,
        ),
        // Imports from a std group.
        (
            "use std::{fs::write, io};\nfn f() { write(\"p\", b\"x\").unwrap(); }".to_owned(),
            1,
        ),
        // A string across lines leaves the rest of its closing line as code.
        ("let s = \"a\nb\"; std::fs::write(\"p\", s).unwrap();".to_owned(), 1),
        // Processes.
        ("std::process::Command::new(\"rm\").arg(p).status()?;".to_owned(), 1),
        ("use std::process::{Command as C, Stdio};".to_owned(), 1),
        // Test code, comments, raw strings and reads.
        (
            format!("fn ok() {{}}\n#[cfg(test)]\nmod tests {{ fn t() {{ {write} }} }}"),
            0,
        ),
        (
            format!(
                "#[cfg(test)]\nmod tests {{\n    const S: &str = \"}}\";\n    fn t() {{ {write} }}\n}}\nfn clean() {{}}"
            ),
            0,
        ),
        (format!("#![cfg(test)]\nfn t() {{ {write} }}"), 0),
        (format!("#[cfg(all(test, unix))]\nfn t() {{ {write} }}"), 0),
        ("#[cfg(test)]\nuse std::fs::write;\nfn f() {}".to_owned(), 0),
        (
            format!("#[cfg(test)]\nimpl<A, B> T for (A, B) {{ fn t() {{ {write} }} }}"),
            0,
        ),
        (
            "/* fs::write(\n /* nested */ fs::rename( */ fn f() {}".to_owned(),
            0,
        ),
        ("let s = r#\"std::fs::write(\"x\")\"#;".to_owned(), 0),
        ("let s = r##\"a\"#\nfs::write(\n\"##;".to_owned(), 0),
        (
            "let fd = rustix::fs::open(p, OFlags::RDONLY | OFlags::CLOEXEC, Mode::empty())?;"
                .to_owned(),
            0,
        ),
        (
            "fn f<'a>(x: &'a str) { let q = '\"'; let s = \"fs::write(\"; }".to_owned(),
            0,
        ),
        ("use std::os::unix::process::CommandExt;".to_owned(), 0),
        ("let reuse = fs::read(p)?; fn use_it() {}".to_owned(), 0),
    ] {
        assert_eq!(
            scan_source("src/x.rs", &source).len(),
            expected,
            "{source:?}: {:?}",
            scan_source("src/x.rs", &source)
        );
    }
}

#[test]
fn the_lexer_blanks_literals_and_drops_comments_across_lines() {
    for (source, expected) in [
        ("  // all comment", vec!["  "]),
        ("a /* b /* c */ d */ e", vec!["a  e"]),
        ("a /* b\nc */ d", vec!["a ", " d"]),
        ("let s = \"x\\\"y\"; z", vec!["let s = \"\"; z"]),
        ("let s = \"a\nb\"; z", vec!["let s = \"", "\"; z"]),
        ("r##\"a \"# b\"## c", vec!["r\"\" c"]),
        ("br\"x\" cr#\"y\"#", vec!["br\"\" cr\"\""]),
        ("let r#type = 1;", vec!["let r#type = 1;"]),
        ("fn f<'a>(x: &'a str) {}", vec!["fn f<'a>(x: &'a str) {}"]),
        (
            "let c = '\"'; let d = '\\''; let e = '\\u{7f}';",
            vec!["let c = ''; let d = ''; let e = '';"],
        ),
        ("'outer: loop { b'x' }", vec!["'outer: loop { b'' }"]),
        ("a\r\nb", vec!["a", "b"]),
    ] {
        assert_eq!(code_lines(source), expected, "{source:?}");
    }
}

/// `src/net/mod.rs` declares `mod tests;` before its production code: every line after
/// it is scanned, through the last.
#[test]
fn production_code_after_a_test_module_declaration_is_scanned() {
    let text = std::fs::read_to_string(root().join("src/net/mod.rs")).unwrap();
    let lines: Vec<usize> = production_lines(&text).iter().map(|(n, _)| *n).collect();
    let declared = text.lines().position(|l| l.trim() == "mod tests;").unwrap() + 1;
    assert!(!lines.contains(&declared) && !lines.contains(&(declared - 1)));
    let next = declared + 1;
    let last = text
        .lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
        .last()
        .unwrap()
        .0
        + 1;
    assert!(lines.contains(&next) && lines.contains(&last), "{lines:?}");
    // A write in the file's last function is flagged.
    let mut injected: Vec<&str> = text.lines().collect();
    injected.insert(last - 1, "std::fs::write(\"x\", \"y\").ok();");
    assert_eq!(
        scan_source("src/net/mod.rs", &injected.join("\n")),
        [format!("src/net/mod.rs:{last}: fs::write(")]
    );
}

#[test]
fn only_fsx_writes_to_the_filesystem() {
    let fsx = root().join("src/fsx");
    let mut violations = Vec::new();
    for file in rust_files(&root().join("src")) {
        if file.starts_with(&fsx) {
            continue;
        }
        let text = std::fs::read_to_string(&file).unwrap();
        violations.extend(scan_source(&label(&file), &text));
    }
    assert!(
        violations.is_empty(),
        "filesystem writes or process spawns outside their allowlists (move writes into fsx):\n{}",
        violations.join("\n")
    );
    for (file, reason) in SPAWNERS {
        assert!(root().join(file).is_file() && !reason.is_empty(), "{file}");
    }
}

/// Modules allowed to call `prune()`, each with its reason. Doctor fixers must never
/// appear here (vision §5.5): the doctor removes by renaming into quarantine.
const PRUNE_CALLERS: &[(&str, &str)] = &[
    (
        "src/engine/tx.rs",
        "post-commit cleanup of stage dirs and git temp checkouts",
    ),
    (
        "src/engine/journal.rs",
        "committed journals, at finish and in recovery",
    ),
    (
        "src/engine/retention.rs",
        "snapshots, and the orphan sweep of stage dirs and in-flight locks",
    ),
    (
        "src/engine/blobs.rs",
        "blob eviction, and temp blobs that lost a same-hash race",
    ),
    (
        "src/backup/create.rs",
        "a capture's own stage and in-flight lock, under its own lock",
    ),
];

#[test]
fn prune_is_called_only_from_its_allowlist() {
    let allowed: Vec<&str> = PRUNE_CALLERS.iter().map(|(file, _)| *file).collect();
    assert!(!allowed.iter().any(|file| file.contains("doctor")));
    let mut calls = Vec::new();
    for file in rust_files(&root().join("src")) {
        let name = label(&file);
        if name == "src/fsx/prune.rs" {
            continue;
        }
        let text = std::fs::read_to_string(&file).unwrap();
        for (line, code) in production_lines(&text) {
            let calls_prune = code.match_indices("prune(").any(|(at, _)| {
                code.get(..at)
                    .and_then(|before| before.chars().last())
                    .is_none_or(|c| !(c.is_alphanumeric() || c == '_'))
            });
            if calls_prune && !allowed.contains(&name.as_str()) {
                calls.push(format!("{name}:{line}"));
            }
        }
    }
    assert!(
        calls.is_empty(),
        "prune() called outside the allowlist:\n{}",
        calls.join("\n")
    );
}
