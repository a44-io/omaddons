//! WowAce's Subversion server and a project's packaging files as they might be: any bytes
//! read as a directory listing, a tag list, `.pkgmeta` or a file's keywords, to values or
//! a typed error, never a panic. Every name a listing or a tag list gives is a plain name,
//! the newest stable tag is one of the tags, and only a TOC's lines carry
//! `@project-version@`.
#![no_main]

use libfuzzer_sys::fuzz_target;
use omaddons::sources::wowace::listing::{
    newest_stable_tag, parse_listing, parse_pkgmeta, parse_tag_revs, scan_keywords,
};

fn plain(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && !name.contains('/')
        && !name.contains("..")
        && !name.chars().any(char::is_control)
}

fuzz_target!(|data: &[u8]| {
    if let Ok(entries) = parse_listing(data) {
        assert!(entries.iter().all(|e| plain(&e.name)));
    }
    if let Ok(tags) = parse_tag_revs(data) {
        assert!(tags.iter().all(|t| plain(&t.tag)));
        if let Some(newest) = newest_stable_tag(&tags) {
            assert!(tags.contains(newest));
        }
    }
    let _ = parse_pkgmeta(&String::from_utf8_lossy(data));
    for path in ["a.toc", "a.lua", "a.xml", "a.txt"] {
        let scan = scan_keywords(path, data);
        assert!(path == "a.toc" || scan.project_version_lines.is_empty());
        assert!(scan.project_version_lines.iter().all(|line| *line >= 1));
    }
});
