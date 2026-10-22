//! The WowAce source (M3-D15): projects from WowAce's public Subversion repository,
//! every tree fetched at one immutable revision, only the packaging subset a source tree
//! can honour without the packager. What the server and a project's files say is read by
//! [`listing`].
//!
//! `fetch` asks one `PROPFIND` of `tags/` for every tag's `version-name`, the revision at
//! which that tag directory last changed: the tag's identity, which a commit elsewhere in
//! the repository never moves (so it never shows as an update), and the revision its tree
//! is fetched at. A `#r<rev>` ref names a trunk revision and costs no request. The release
//! is one [`AssetLocation::WowAceTree`]; the planner calls [`assemble`], which lists and
//! fetches every directory and file under the pinned `!svn/bc/<rev>/` path (never a live
//! one, so a tag edited mid-fetch cannot mix revisions), refuses what needs the packager,
//! and packs the tree into a deterministic zip for the blob cache. From there it is an
//! archive like any other: inspected, verified by its TOCs, staged.

pub mod listing;

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io::{Cursor, Read, Write};

use serde_json::json;

use self::listing::{
    ListingError, TagRev, newest_stable_tag, parse_listing, parse_pkgmeta, parse_tag_revs,
    scan_keywords, unescape_percent,
};
use super::{Source, SourceId, Wanted};
use crate::error::{Error, ErrorCode, Result, Suggestion};
use crate::limits::{
    WOWACE_TREE_MAX_DEPTH, WOWACE_TREE_MAX_DIRS, WOWACE_TREE_MAX_FILES, WOWACE_TREE_MAX_REQUESTS,
};
use crate::model::{
    Asset, AssetLocation, Candidate, Channel, FolderName, RankReason, Ref, Release, ReleaseSet,
    WowAceRev, is_wowace_slug,
};
use crate::net::{Http, Request, Response};

/// Where every project's repository is.
const REPOS: &str = "https://repos.wowace.com/wow";

/// The packager's version keyword, the only one a TOC may hold.
const PROJECT_VERSION: &[u8] = b"@project-version@";

/// `details.reason` of a project refused for needing the packager.
const PACKAGING_UNSUPPORTED: &str = "packaging_unsupported";

/// The `wowace:` source.
#[derive(Debug, Default)]
pub struct WowAceSource;

impl Source for WowAceSource {
    fn id(&self) -> SourceId {
        SourceId::Wowace
    }

    fn fetch(&self, wanted: &[Wanted], http: &dyn Http) -> Vec<Result<ReleaseSet>> {
        // One tag list per project, however many wishes name it.
        let mut lists: BTreeMap<String, Result<Vec<TagRev>>> = BTreeMap::new();
        wanted
            .iter()
            .map(|w| {
                let Ref::WowAce { slug, rev } = &w.r#ref else {
                    return Err(Error::internal(format!(
                        "the wowace source got {}",
                        w.r#ref
                    )));
                };
                // `--version` names what the ref does not (install checked they agree).
                let rev = rev
                    .clone()
                    .or_else(|| w.version.as_deref().and_then(WowAceRev::parse));
                let mut tags = || {
                    lists
                        .entry(slug.clone())
                        .or_insert_with(|| tag_list(http, slug))
                        .clone()
                };
                let releases = match rev {
                    Some(WowAceRev::Revision(rev)) => vec![trunk(slug, rev)],
                    Some(WowAceRev::Tag(tag)) => {
                        let tags = tags()?;
                        let found = find_tag(&tags, &tag).ok_or_else(|| missing_tag(slug, &tag))?;
                        vec![tagged(slug, found)]
                    }
                    // No stable tag at all: an empty set, which selection reports.
                    None => newest_stable_tag(&tags()?)
                        .map(|t| tagged(slug, t))
                        .into_iter()
                        .collect(),
                };
                Ok(ReleaseSet {
                    r#ref: w.r#ref.clone(),
                    releases,
                })
            })
            .collect()
    }
}

/// The project's tags with their revisions: one `PROPFIND` of `tags/`.
fn tag_list(http: &dyn Http, slug: &str) -> Result<Vec<TagRev>> {
    let url = format!("{REPOS}/{slug}/tags/");
    let resp = http.send(Request::propfind_version_names(url, "wowace-tags"))?;
    if resp.status == 404 {
        return Err(Error::new(
            ErrorCode::NotFound,
            format!("WowAce has no project wowace:{slug}"),
            Suggestion::text("check the project's slug: its WowAce page's URL ends with it"),
        )
        .details(json!({ "ref": format!("wowace:{slug}") })));
    }
    let body = resp.error_for_status("the WowAce tag list")?.bytes()?;
    parse_tag_revs(&body).map_err(|e| unreadable(&e, &format!("wowace:{slug}'s tag list")))
}

/// The tag named `tag`, a leading `v` tolerated on either side when no tag is named
/// exactly so.
fn find_tag<'a>(tags: &'a [TagRev], tag: &str) -> Option<&'a TagRev> {
    let bare = |s: &str| s.strip_prefix(['v', 'V']).unwrap_or(s).to_owned();
    tags.iter()
        .find(|t| t.tag == tag)
        .or_else(|| tags.iter().find(|t| bare(&t.tag) == bare(tag)))
}

fn missing_tag(slug: &str, tag: &str) -> Error {
    Error::new(
        ErrorCode::NotFound,
        format!("wowace:{slug} has no tag {tag}"),
        Suggestion::text(format!(
            "check the tag's name in the list at {REPOS}/{slug}/tags/"
        )),
    )
    .details(json!({ "ref": format!("wowace:{slug}"), "version": tag }))
}

/// The candidate of a release recorded at `revision` (a lock, an exact reinstall):
/// `version` (a tag, or `r<rev>` for a trunk peg) as of that revision, built with no
/// request, so the tree is the recorded one whether or not the tag moved since. `None`
/// when `version` names neither, or a trunk peg at another revision.
pub fn recorded_candidate(
    slug: &str,
    version: &str,
    revision: u32,
    channel: Channel,
) -> Option<Candidate> {
    let release = match WowAceRev::parse(version)? {
        WowAceRev::Revision(rev) if rev == revision => trunk(slug, rev),
        WowAceRev::Revision(_) => return None,
        WowAceRev::Tag(tag) => tagged(slug, &TagRev { tag, rev: revision }),
    };
    let asset = release.assets.into_iter().next()?;
    Some(Candidate {
        release_id: release.release_id,
        version: release.version,
        channel,
        asset,
        rank: RankReason::Unannotated,
        revision: Some(revision),
    })
}

/// The release of `tag` at its last-changed revision.
fn tagged(slug: &str, tag: &TagRev) -> Release {
    release(
        format!("wowace:{slug}@{}", tag.tag),
        tag.tag.clone(),
        tree_asset(slug, &format!("tags/{}", tag.tag), tag.rev),
        tag.rev,
    )
}

/// The release of trunk as of `rev` (`#r<rev>`).
fn trunk(slug: &str, rev: u32) -> Release {
    release(
        format!("wowace:{slug}@r{rev}"),
        format!("r{rev}"),
        tree_asset(slug, "trunk", rev),
        rev,
    )
}

fn release(release_id: String, version: String, asset: Asset, rev: u32) -> Release {
    Release {
        release_id,
        version,
        // A named tag is what was asked for, whatever its name says.
        channel: Channel::Stable,
        published_at: None,
        assets: vec![asset],
        changelog_url: None,
        notes: None,
        name_hint: None,
        revision: Some(rev),
    }
}

/// The tree at `path` (`tags/<tag>` or `trunk`) as of `rev`. Its file name keys the
/// candidate facts and the command's downloads, so a re-tag never reuses the old tree.
fn tree_asset(slug: &str, path: &str, rev: u32) -> Asset {
    Asset {
        filename: format!("{slug}/{path}@r{rev}"),
        location: AssetLocation::WowAceTree {
            base: format!("{REPOS}/{slug}/!svn/bc/{rev}/{}/", encode_path(path)),
            revision: rev,
        },
        size: None,
        expected_hash: None,
        flavors: Vec::new(),
        name_flavors: Vec::new(),
        nolib: false,
    }
}

/// A repository path for a URL: each segment's bytes outside the unreserved set (and
/// `!`, common in addon names) percent-encoded, the slashes kept.
fn encode_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for b in path.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~!/".contains(&b) {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// What a [`AssetLocation::WowAceTree`] names, checked: its project, and the label
/// `@project-version@` becomes (the tag, or `r<rev>` for trunk).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Pinned {
    slug: String,
    label: String,
}

/// `base` read back: `<repos>/<slug>/!svn/bc/<revision>/(tags/<tag>|trunk)/`, its
/// revision `revision`. A location in any other shape (a hand-edited memo) is refused,
/// so no live path is ever fetched.
fn pinned(base: &str, revision: u32) -> Result<Pinned> {
    let refuse = || Error::internal(format!("{base} is not a pinned WowAce tree at r{revision}"));
    let rest = base
        .strip_prefix(REPOS)
        .and_then(|r| r.strip_prefix('/'))
        .ok_or_else(refuse)?;
    let (slug, rest) = rest.split_once("/!svn/bc/").ok_or_else(refuse)?;
    let rest = rest
        .strip_prefix(&format!("{revision}/"))
        .ok_or_else(refuse)?;
    if !is_wowace_slug(slug) {
        return Err(refuse());
    }
    let label = match rest {
        "trunk/" => format!("r{revision}"),
        _ => {
            let tag = rest
                .strip_prefix("tags/")
                .and_then(|t| t.strip_suffix('/'))
                .filter(|t| !t.is_empty() && !t.contains('/'))
                .ok_or_else(refuse)?;
            unescape_percent(tag)
        }
    };
    Ok(Pinned {
        slug: slug.to_owned(),
        label,
    })
}

/// When `error` is [`assemble`] refusing a project that needs the packager, the reason a
/// candidate's `tried[]` entry gives (`packaging_unsupported: .pkgmeta: externals; …`):
/// the candidate is refused like one that does not load here, not failed to read.
pub fn packaging_refusal(error: &Error) -> Option<String> {
    let refused = error.code == ErrorCode::BadArchive
        && matches!(
            error.details["reason"].as_str(),
            Some(PACKAGING_UNSUPPORTED)
        );
    if !refused {
        return None;
    }
    let named: Vec<&str> = error.details["unsupported"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    Some(format!("{PACKAGING_UNSUPPORTED}: {}", named.join("; ")))
}

/// The tree at `base` (a [`AssetLocation::WowAceTree`]), fetched at `revision` and built
/// as the packager would build a project that needs nothing but `package-as` and
/// `@project-version@`: every file under one folder (`package-as`, else the slug),
/// dotfiles left out, `@project-version@` in each TOC replaced with the tag (`r<rev>` for
/// trunk), packed as a deterministic zip (entries sorted, every timestamp
/// 1980-01-01T00:00:00, Deflate, mode 0644), so one pinned tree always packs to the same
/// bytes.
///
/// Every listing and file is fetched under `base`. The walk is bounded by
/// `WOWACE_TREE_MAX_*` and `max_bytes` (`archive_too_large`, the bound named), all
/// directories listed before any file is fetched. Names that collide ignoring case are
/// `bad_archive` (`tree_collision`); a `.pkgmeta` key, `package-as` or keyword outside the
/// subset is `bad_archive` (`packaging_unsupported`), naming each.
pub fn assemble(http: &dyn Http, base: &str, revision: u32, max_bytes: u64) -> Result<Vec<u8>> {
    let pinned = pinned(base, revision)?;
    let mut walk = Walk {
        http,
        base,
        requests: 0,
    };
    let paths = walk.list()?;
    let pkgmeta = walk.pkgmeta()?;
    let mut files = BTreeMap::new();
    let mut bytes = 0u64;
    let mut metas = pkgmeta.into_iter().collect::<Vec<_>>();
    for path in paths {
        let body = walk.file(&path, max_bytes.saturating_sub(bytes), max_bytes)?;
        bytes = bytes.saturating_add(u64::try_from(body.len()).unwrap_or(u64::MAX));
        // The packager also reads these names; they are never packaged.
        if path.eq_ignore_ascii_case("pkgmeta.yaml") || path.eq_ignore_ascii_case("pkgmeta.yml") {
            metas.push((path, body));
            continue;
        }
        files.insert(path, body);
    }
    let root = packaging(&pinned, &metas, &mut files)?;
    pack(&root, &files)
}

/// A tree's requests, all under `base`, counted with their retries.
struct Walk<'a> {
    http: &'a dyn Http,
    base: &'a str,
    requests: u32,
}

impl Walk<'_> {
    /// One request for `path` below the root, within `WOWACE_TREE_MAX_REQUESTS`.
    fn get(&mut self, path: &str, purpose: &'static str, download: bool) -> Result<Response> {
        if self.requests >= WOWACE_TREE_MAX_REQUESTS {
            return Err(bound(
                "WOWACE_TREE_MAX_REQUESTS",
                u64::from(WOWACE_TREE_MAX_REQUESTS),
                "requests",
            ));
        }
        let mut req = Request::get(format!("{}{}", self.base, encode_path(path)), purpose);
        if download {
            req = req.download();
        }
        let resp = self.http.send(req)?;
        self.requests = self.requests.saturating_add(resp.attempts());
        if self.requests > WOWACE_TREE_MAX_REQUESTS {
            return Err(bound(
                "WOWACE_TREE_MAX_REQUESTS",
                u64::from(WOWACE_TREE_MAX_REQUESTS),
                "requests",
            ));
        }
        Ok(resp)
    }

    /// Every file's path below the root, sorted, from listing each directory breadth
    /// first.
    fn list(&mut self) -> Result<Vec<String>> {
        let mut queue = VecDeque::from([(String::new(), 0usize)]);
        let mut listed = 0usize;
        let mut folded = BTreeSet::new();
        let mut files = Vec::new();
        while let Some((dir, depth)) = queue.pop_front() {
            if listed == WOWACE_TREE_MAX_DIRS {
                return Err(bound(
                    "WOWACE_TREE_MAX_DIRS",
                    WOWACE_TREE_MAX_DIRS as u64,
                    "directories",
                ));
            }
            listed += 1;
            let body = self
                .get(&dir, "wowace-tree", false)?
                .error_for_status("a WowAce listing")?
                .bytes()?;
            let entries = parse_listing(&body).map_err(|e| match e {
                ListingError::DuplicateName { name } => collision(&format!("{dir}{name}")),
                e => unreadable(&e, &format!("the listing of /{dir}")),
            })?;
            for entry in entries {
                let path = format!("{dir}{}", entry.name);
                if !folded.insert(path.to_lowercase()) {
                    return Err(collision(&path));
                }
                if entry.dir {
                    if depth + 1 > WOWACE_TREE_MAX_DEPTH {
                        return Err(bound(
                            "WOWACE_TREE_MAX_DEPTH",
                            WOWACE_TREE_MAX_DEPTH as u64,
                            "directory levels",
                        ));
                    }
                    queue.push_back((format!("{path}/"), depth + 1));
                } else {
                    if files.len() == WOWACE_TREE_MAX_FILES {
                        return Err(bound(
                            "WOWACE_TREE_MAX_FILES",
                            WOWACE_TREE_MAX_FILES as u64,
                            "files",
                        ));
                    }
                    files.push(path);
                }
            }
        }
        files.sort();
        Ok(files)
    }

    /// The root's `.pkgmeta`, if it has one (a dotfile: no listing names it).
    fn pkgmeta(&mut self) -> Result<Option<(String, Vec<u8>)>> {
        let resp = self.get(".pkgmeta", "wowace-file", false)?;
        if resp.status == 404 {
            return Ok(None);
        }
        let body = resp.error_for_status("a WowAce file")?.bytes()?;
        Ok(Some((".pkgmeta".to_owned(), body)))
    }

    /// The file at `path`, at most the `remaining` of the tree's `max_bytes`.
    fn file(&mut self, path: &str, remaining: u64, max_bytes: u64) -> Result<Vec<u8>> {
        let resp = self
            .get(path, "wowace-file", true)?
            .error_for_status("a WowAce file")?;
        let mut body = Vec::new();
        resp.into_reader()
            .take(remaining.saturating_add(1))
            .read_to_end(&mut body)
            .map_err(crate::net::body_error)?;
        if u64::try_from(body.len()).unwrap_or(u64::MAX) > remaining {
            return Err(crate::net::download_too_large(max_bytes));
        }
        Ok(body)
    }
}

/// Checks the packaging subset and applies it: the root folder (`package-as`, else the
/// slug), and `@project-version@` replaced in each TOC. Every refusal is collected, so
/// one answer names them all.
fn packaging(
    pinned: &Pinned,
    metas: &[(String, Vec<u8>)],
    files: &mut BTreeMap<String, Vec<u8>>,
) -> Result<FolderName> {
    let mut unsupported = Vec::new();
    let mut package_as = None;
    for (name, body) in metas {
        let meta = parse_pkgmeta(&String::from_utf8_lossy(body));
        unsupported.extend(meta.unsupported.iter().map(|key| format!("{name}: {key}")));
        if let Some(folder) = meta.package_as {
            match FolderName::new(&folder) {
                Ok(folder) => package_as = Some(folder),
                Err(_) => unsupported.push(format!(
                    "{name}: package-as {folder:?} is not a folder name"
                )),
            }
        }
    }
    for (path, body) in files.iter_mut() {
        let ext = path.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase());
        let Some(ext) = ext.filter(|e| matches!(e.as_str(), "toc" | "lua" | "xml")) else {
            continue;
        };
        let scan = scan_keywords(path, body);
        unsupported.extend(scan.unsupported);
        if ext == "toc" && !scan.project_version_lines.is_empty() {
            *body = replace(body, PROJECT_VERSION, pinned.label.as_bytes());
        }
    }
    if !unsupported.is_empty() {
        return Err(Error::new(
            ErrorCode::BadArchive,
            format!("needs the packager: {}", unsupported.join("; ")),
            Suggestion::text(
                "download a packaged release of the addon in a browser, then install it from the file",
            ),
        )
        .details(json!({ "reason": PACKAGING_UNSUPPORTED, "unsupported": unsupported })));
    }
    match package_as {
        Some(folder) => Ok(folder),
        None => FolderName::new(&pinned.slug)
            .map_err(|e| Error::internal(format!("wowace:{}: {e}", pinned.slug))),
    }
}

/// `body` with every `from` replaced by `to`, byte for byte (a TOC need not be UTF-8).
fn replace(body: &[u8], from: &[u8], to: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len());
    let mut rest = body;
    while let Some(at) = rest.windows(from.len()).position(|w| w == from) {
        out.extend_from_slice(&rest[..at]);
        out.extend_from_slice(to);
        rest = &rest[at + from.len()..];
    }
    out.extend_from_slice(rest);
    out
}

/// The deterministic zip of `files` under `root/`.
fn pack(root: &FolderName, files: &BTreeMap<String, Vec<u8>>) -> Result<Vec<u8>> {
    use zip::write::SimpleFileOptions;
    let failed = |e: &dyn std::fmt::Display| Error::internal(format!("packing a WowAce tree: {e}"));
    let options = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .last_modified_time(zip::DateTime::default())
        .unix_permissions(0o644);
    // Sorted by the entry's full name: one root, so the paths' own order.
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (path, body) in files {
        zip.start_file(format!("{}/{path}", root.as_str()), options)
            .map_err(|e| failed(&e))?;
        zip.write_all(body).map_err(|e| failed(&e))?;
    }
    Ok(zip.finish().map_err(|e| failed(&e))?.into_inner())
}

/// `archive_too_large` for a tree past one of its bounds, named.
fn bound(name: &str, max: u64, what: &str) -> Error {
    Error::new(
        ErrorCode::ArchiveTooLarge,
        format!("the WowAce tree is larger than {name} allows ({max} {what})"),
        Suggestion::text(
            "download a packaged release of the addon in a browser, then install it from the file",
        ),
    )
    .details(json!({ "limit": max, "constant": name }))
}

/// `bad_archive` (`tree_collision`): two names in one tree equal ignoring case, which
/// WoW, loading folders case-insensitively, cannot tell apart.
fn collision(path: &str) -> Error {
    Error::new(
        ErrorCode::BadArchive,
        format!("the WowAce tree names /{path} twice, ignoring case"),
        Suggestion::text("retry later; if it persists, the project's tree is broken upstream"),
    )
    .details(json!({ "reason": "tree_collision", "path": path }))
}

/// `bad_archive` for an answer the listing parser refused.
fn unreadable(error: &ListingError, what: &str) -> Error {
    if let ListingError::TooManyEntries { max } = error {
        return bound("WOWACE_TREE_MAX_FILES", *max as u64, "entries");
    }
    Error::new(
        ErrorCode::BadArchive,
        format!("WowAce sent {what} that cannot be read: {error}"),
        Suggestion::text("retry later; if it persists, the WowAce server's answers have changed"),
    )
    .details(json!({ "reason": error.wire() }))
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    /// Answers by URL; records every request as `METHOD url`.
    #[derive(Default)]
    struct Canned {
        answers: BTreeMap<String, (u16, Vec<u8>, u32)>,
        seen: Mutex<Vec<String>>,
    }

    impl Canned {
        fn with(mut self, url: &str, status: u16, body: &str) -> Self {
            self.answers
                .insert(url.to_owned(), (status, body.as_bytes().to_vec(), 1));
            self
        }

        /// An answer that took `attempts` wire requests.
        fn retried(mut self, url: &str, body: &str, attempts: u32) -> Self {
            self.answers
                .insert(url.to_owned(), (200, body.as_bytes().to_vec(), attempts));
            self
        }

        fn seen(&self) -> Vec<String> {
            self.seen.lock().unwrap().clone()
        }
    }

    impl Http for Canned {
        fn send(&self, req: Request) -> Result<Response> {
            self.seen
                .lock()
                .unwrap()
                .push(format!("{} {}", req.method.as_str(), req.url));
            let Some((status, body, attempts)) = self.answers.get(&req.url).cloned() else {
                return Ok(Response::new(
                    404,
                    Vec::new(),
                    req.url,
                    Box::new(std::io::empty()),
                ));
            };
            let mut resp = Response::new(status, Vec::new(), req.url, Box::new(Cursor::new(body)));
            resp.attempts = attempts;
            Ok(resp)
        }
    }

    const BASE: &str = "https://repos.wowace.com/wow/bug-grabber/!svn/bc/401/tags/v12.1.0/";

    fn index(entries: &[&str]) -> String {
        let items: String = entries
            .iter()
            .map(|e| format!("<li><a href=\"{e}\">{e}</a></li>\n"))
            .collect();
        format!(
            "<html><body><h2>x - Revision 401: /</h2><ul>\n\
             <li><a href=\"../\">..</a></li>\n{items}</ul></body></html>"
        )
    }

    /// BugGrabber's v12.1.0 tree, as the server lists it.
    fn bug_grabber() -> Canned {
        Canned::default()
            .with(
                BASE,
                200,
                &index(&[
                    "!BugGrabber.toc",
                    ".luacheckrc",
                    ".pkgmeta",
                    "BugGrabber.lua",
                    "load.xml",
                ]),
            )
            .with(
                &format!("{BASE}.pkgmeta"),
                200,
                "package-as: \"!BugGrabber\"\n",
            )
            .with(
                &format!("{BASE}!BugGrabber.toc"),
                200,
                "## Interface: 120001\n## Version: @project-version@\nload.xml\n",
            )
            .with(&format!("{BASE}BugGrabber.lua"), 200, "local x = 1\n")
            .with(&format!("{BASE}load.xml"), 200, "<Ui></Ui>\n")
    }

    fn entries(zip: &[u8]) -> BTreeMap<String, String> {
        let mut archive = zip::ZipArchive::new(Cursor::new(zip.to_vec())).unwrap();
        (0..archive.len())
            .map(|i| {
                let mut file = archive.by_index(i).unwrap();
                let mut text = String::new();
                file.read_to_string(&mut text).unwrap();
                (file.name().unwrap().into_owned(), text)
            })
            .collect()
    }

    #[test]
    fn a_tree_is_fetched_pinned_packaged_and_packed() {
        let http = bug_grabber();
        let zip = assemble(&http, BASE, 401, 1 << 20).unwrap();
        let files = entries(&zip);
        assert_eq!(
            files.keys().map(String::as_str).collect::<Vec<_>>(),
            [
                "!BugGrabber/!BugGrabber.toc",
                "!BugGrabber/BugGrabber.lua",
                "!BugGrabber/load.xml"
            ],
            "under package-as, sorted, dotfiles left out"
        );
        assert!(
            files["!BugGrabber/!BugGrabber.toc"].contains("## Version: v12.1.0\n"),
            "{files:?}"
        );
        let seen = http.seen();
        assert!(
            seen.iter().all(|s| s.starts_with(&format!("GET {BASE}"))),
            "every request under the pinned base: {seen:?}"
        );
        assert_eq!(
            seen.len(),
            5,
            "one listing, .pkgmeta, three files: {seen:?}"
        );

        let again = assemble(&bug_grabber(), BASE, 401, 1 << 20).unwrap();
        assert_eq!(again, zip, "one pinned tree packs to the same bytes");
        let mut archive = zip::ZipArchive::new(Cursor::new(zip)).unwrap();
        let first = archive.by_index(0).unwrap();
        assert_eq!(first.last_modified(), Some(zip::DateTime::default()));
        assert_eq!(first.unix_mode().map(|m| m & 0o777), Some(0o644));
        assert_eq!(first.compression(), zip::CompressionMethod::Deflated);
    }

    #[test]
    fn a_trunk_peg_without_pkgmeta_installs_as_its_slug_versioned_by_revision() {
        let base = "https://repos.wowace.com/wow/bug-grabber/!svn/bc/152/trunk/";
        let http = Canned::default()
            .with(base, 200, &index(&["BugGrabber.toc", "Libs/"]))
            .with(
                &format!("{base}BugGrabber.toc"),
                200,
                "## Version: @project-version@\n",
            )
            .with(&format!("{base}Libs/"), 200, &index(&["a b.lua"]))
            .with(&format!("{base}Libs/a%20b.lua"), 200, "-- a\n");
        let files = entries(&assemble(&http, base, 152, 1 << 20).unwrap());
        assert_eq!(files["bug-grabber/BugGrabber.toc"], "## Version: r152\n");
        assert_eq!(files["bug-grabber/Libs/a b.lua"], "-- a\n");
    }

    #[test]
    fn only_pinned_locations_are_fetched() {
        for (base, rev) in [
            (
                "https://repos.wowace.com/wow/bug-grabber/tags/v12.1.0/",
                401,
            ),
            (
                "https://repos.wowace.com/wow/bug-grabber/!svn/bc/400/tags/v12.1.0/",
                401,
            ),
            (
                "https://repos.wowace.com/wow/bug-grabber/!svn/bc/401/branches/x/",
                401,
            ),
            (
                "https://repos.wowace.com/wow/Bug_Grabber/!svn/bc/401/trunk/",
                401,
            ),
            (
                "https://example.org/wow/bug-grabber/!svn/bc/401/trunk/",
                401,
            ),
            (
                "https://repos.wowace.com/wow/bug-grabber/!svn/bc/401/tags/a/b/",
                401,
            ),
        ] {
            let http = Canned::default();
            let err = assemble(&http, base, rev, 1 << 20).unwrap_err();
            assert_eq!(err.code, ErrorCode::Internal, "{base}");
            assert!(http.seen().is_empty(), "{base}: nothing requested");
        }
        assert_eq!(
            pinned("https://repos.wowace.com/wow/x/!svn/bc/7/tags/v1%201/", 7).unwrap(),
            Pinned {
                slug: "x".into(),
                label: "v1 1".into()
            }
        );
    }

    #[test]
    fn what_needs_the_packager_is_refused_by_name() {
        let http = bug_grabber()
            .with(
                &format!("{BASE}.pkgmeta"),
                200,
                "package-as: \"!BugGrabber\"\nexternals:\n  Libs/X: url\nmove-folders:\n  a: b\n",
            )
            .with(
                &format!("{BASE}BugGrabber.lua"),
                200,
                "local x = 1\n--@debug@\nprint(1)\n--@end-debug@\n",
            );
        let err = assemble(&http, BASE, 401, 1 << 20).unwrap_err();
        assert_eq!(
            packaging_refusal(&err).as_deref(),
            Some(
                "packaging_unsupported: .pkgmeta: externals; .pkgmeta: move-folders; \
                 BugGrabber.lua:2: --@debug@; BugGrabber.lua:4: --@end-debug@"
            )
        );
        let named: Vec<&str> = err.details["unsupported"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert_eq!(
            named,
            [
                ".pkgmeta: externals",
                ".pkgmeta: move-folders",
                "BugGrabber.lua:2: --@debug@",
                "BugGrabber.lua:4: --@end-debug@",
            ]
        );
        assert!(
            err.message
                .starts_with("needs the packager: .pkgmeta: externals"),
            "{}",
            err.message
        );

        let http = bug_grabber().with(&format!("{BASE}.pkgmeta"), 200, "package-as: \".Hidden\"\n");
        let err = assemble(&http, BASE, 401, 1 << 20).unwrap_err();
        assert!(packaging_refusal(&err).is_some(), "{err:?}");
        assert!(
            err.message
                .contains("package-as \".Hidden\" is not a folder name"),
            "{}",
            err.message
        );
        assert_eq!(
            packaging_refusal(&collision("a")),
            None,
            "a collision is not a refusal"
        );

        let http = bug_grabber()
            .with(
                BASE,
                200,
                &index(&[
                    "!BugGrabber.toc",
                    "pkgmeta.yaml",
                    "BugGrabber.lua",
                    "load.xml",
                ]),
            )
            .with(&format!("{BASE}pkgmeta.yaml"), 200, "ignore:\n  - x\n");
        let err = assemble(&http, BASE, 401, 1 << 20).unwrap_err();
        assert_eq!(err.details["unsupported"][0], "pkgmeta.yaml: ignore");
    }

    #[test]
    fn names_equal_ignoring_case_are_a_collision() {
        let http = bug_grabber().with(
            BASE,
            200,
            &index(&["!BugGrabber.toc", "load.xml", "Load.XML"]),
        );
        let err = assemble(&http, BASE, 401, 1 << 20).unwrap_err();
        assert_eq!(
            (err.code, err.details["reason"].as_str()),
            (ErrorCode::BadArchive, Some("tree_collision"))
        );
        let http = bug_grabber().with(BASE, 200, &index(&["Libs/", "libs/"]));
        let err = assemble(&http, BASE, 401, 1 << 20).unwrap_err();
        assert_eq!(
            err.details["reason"], "tree_collision",
            "a directory listed twice"
        );
    }

    #[test]
    fn the_bounds_fail_before_any_file_is_fetched() {
        // A directory inside itself, all the way down.
        let mut http = Canned::default();
        let mut dir = BASE.to_owned();
        for _ in 0..=WOWACE_TREE_MAX_DEPTH {
            http = http.with(&dir, 200, &index(&["a/", "f.lua"]));
            dir.push_str("a/");
        }
        let err = assemble(&http, BASE, 401, 1 << 20).unwrap_err();
        assert_eq!(
            (err.code, err.details["constant"].as_str()),
            (ErrorCode::ArchiveTooLarge, Some("WOWACE_TREE_MAX_DEPTH"))
        );
        assert!(
            http.seen().iter().all(|s| s.ends_with('/')),
            "no file fetched"
        );

        let subdirs: Vec<String> = (0..WOWACE_TREE_MAX_DIRS)
            .map(|i| format!("d{i}/"))
            .collect();
        let names: Vec<&str> = subdirs.iter().map(String::as_str).collect();
        let mut http = Canned::default().with(BASE, 200, &index(&names));
        for name in &subdirs {
            http = http.with(&format!("{BASE}{name}"), 200, &index(&[]));
        }
        let err = assemble(&http, BASE, 401, 1 << 20).unwrap_err();
        assert_eq!(err.details["constant"], "WOWACE_TREE_MAX_DIRS");
        assert_eq!(
            http.seen().len(),
            WOWACE_TREE_MAX_DIRS,
            "the 201st is never listed"
        );

        // Retries count: a listing that took the whole budget.
        let http = bug_grabber().retried(BASE, &index(&["a.lua"]), WOWACE_TREE_MAX_REQUESTS);
        let err = assemble(&http, BASE, 401, 1 << 20).unwrap_err();
        assert_eq!(err.details["constant"], "WOWACE_TREE_MAX_REQUESTS");
        assert_eq!(http.seen().len(), 1);

        // The download limit, over the whole tree.
        let err = assemble(&bug_grabber(), BASE, 401, 60).unwrap_err();
        assert_eq!(
            (err.code, err.details["config_key"].as_str()),
            (
                ErrorCode::ArchiveTooLarge,
                Some("limits.download_max_bytes")
            )
        );
    }

    #[test]
    fn a_listing_that_is_not_one_is_unreadable() {
        let http = bug_grabber().with(BASE, 200, "<html>login</html>");
        let err = assemble(&http, BASE, 401, 1 << 20).unwrap_err();
        assert_eq!(
            (err.code, err.details["reason"].as_str()),
            (ErrorCode::BadArchive, Some("not_a_listing"))
        );
        let http = bug_grabber().with(BASE, 503, "");
        assert_eq!(
            assemble(&http, BASE, 401, 1 << 20).unwrap_err().code,
            ErrorCode::HttpStatus
        );
    }

    fn propfind(tags: &[(&str, u32)]) -> String {
        let mut out = String::from(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?><D:multistatus xmlns:D=\"DAV:\">\
             <D:response><D:href>/wow/bug-grabber/tags/</D:href><D:propstat><D:prop>\
             <lp1:version-name>403</lp1:version-name></D:prop></D:propstat></D:response>",
        );
        for (tag, rev) in tags {
            out.push_str(&format!(
                "<D:response><D:href>/wow/bug-grabber/tags/{tag}/</D:href><D:propstat><D:prop>\
                 <lp1:version-name>{rev}</lp1:version-name></D:prop></D:propstat></D:response>"
            ));
        }
        out.push_str("</D:multistatus>");
        out
    }

    const TAGS: &str = "https://repos.wowace.com/wow/bug-grabber/tags/";

    fn fetch(http: &dyn Http, wanted: &[Wanted]) -> Vec<Result<ReleaseSet>> {
        WowAceSource.fetch(wanted, http)
    }

    #[test]
    fn fetch_pins_each_ref_form_and_asks_once_per_project() {
        let http = Canned::default().with(
            TAGS,
            207,
            &propfind(&[("v12.0.9", 369), ("v12.1.0", 401), ("v12.2.0-beta1", 402)]),
        );
        let newest = Wanted::stable("wowace:bug-grabber".parse().unwrap());
        let named = Wanted::stable("wowace:bug-grabber#v12.0.9".parse().unwrap());
        let peg = Wanted::stable("wowace:bug-grabber#r152".parse().unwrap());
        let sets = fetch(&http, &[newest, named, peg]);
        let release = |i: usize| sets[i].as_ref().unwrap().releases[0].clone();

        let r = release(0);
        assert_eq!(
            (r.release_id.as_str(), r.version.as_str(), r.revision),
            ("wowace:bug-grabber@v12.1.0", "v12.1.0", Some(401))
        );
        assert_eq!(r.assets[0].filename, "bug-grabber/tags/v12.1.0@r401");
        assert_eq!(
            r.assets[0].location,
            AssetLocation::WowAceTree {
                base: BASE.into(),
                revision: 401
            }
        );
        assert_eq!(release(1).revision, Some(369));
        let r = release(2);
        assert_eq!(
            (r.release_id.as_str(), r.version.as_str(), r.revision),
            ("wowace:bug-grabber@r152", "r152", Some(152))
        );
        assert_eq!(
            r.assets[0].location,
            AssetLocation::WowAceTree {
                base: "https://repos.wowace.com/wow/bug-grabber/!svn/bc/152/trunk/".into(),
                revision: 152
            }
        );
        assert_eq!(
            http.seen(),
            [format!("PROPFIND {TAGS}")],
            "one PROPFIND for both tag wishes, none for the peg"
        );
    }

    #[test]
    fn fetch_maps_missing_tags_projects_and_stable_releases() {
        let http = Canned::default().with(TAGS, 207, &propfind(&[("v1.0-beta1", 5)]));
        let none = Wanted::stable("wowace:bug-grabber".parse().unwrap());
        let missing = Wanted::stable("wowace:bug-grabber#v9".parse().unwrap());
        let mut versioned = Wanted::stable("wowace:bug-grabber".parse().unwrap());
        versioned.version = Some("1.0-beta1".into());
        let sets = fetch(&http, &[none, missing, versioned]);
        assert!(
            sets[0].as_ref().unwrap().releases.is_empty(),
            "no stable tag"
        );
        let err = sets[1].as_ref().unwrap_err();
        assert_eq!(err.code, ErrorCode::NotFound);
        assert_eq!(err.message, "wowace:bug-grabber has no tag v9");
        assert_eq!(
            sets[2].as_ref().unwrap().releases[0].release_id,
            "wowace:bug-grabber@v1.0-beta1",
            "--version names a tag, a leading v tolerated"
        );

        let gone = fetch(
            &Canned::default(),
            &[Wanted::stable("wowace:nope".parse().unwrap())],
        );
        assert_eq!(gone[0].as_ref().unwrap_err().code, ErrorCode::NotFound);
        let broken = fetch(
            &Canned::default().with("https://repos.wowace.com/wow/x/tags/", 207, "<html/>"),
            &[Wanted::stable("wowace:x".parse().unwrap())],
        );
        assert_eq!(
            broken[0].as_ref().unwrap_err().details["reason"],
            "not_a_listing"
        );
    }

    #[test]
    fn a_recorded_release_is_rebuilt_at_its_revision_without_a_request() {
        let c = recorded_candidate("bug-grabber", "v12.1.0", 401, Channel::Stable).unwrap();
        assert_eq!(
            (c.release_id.as_str(), c.version.as_str(), c.revision),
            ("wowace:bug-grabber@v12.1.0", "v12.1.0", Some(401))
        );
        assert_eq!(
            c.asset.location,
            AssetLocation::WowAceTree {
                base: "https://repos.wowace.com/wow/bug-grabber/!svn/bc/401/tags/v12.1.0/".into(),
                revision: 401
            }
        );
        let peg = recorded_candidate("bug-grabber", "r152", 152, Channel::Stable).unwrap();
        assert_eq!(peg.release_id, "wowace:bug-grabber@r152");
        assert!(recorded_candidate("bug-grabber", "r152", 153, Channel::Stable).is_none());
        assert!(recorded_candidate("bug-grabber", "", 1, Channel::Stable).is_none());
    }

    #[test]
    fn replace_swaps_every_occurrence_bytewise() {
        assert_eq!(replace(b"a@x@b@x@", b"@x@", b"\xff"), b"a\xffb\xff");
        assert_eq!(replace(b"none", b"@x@", b"y"), b"none");
        assert_eq!(encode_path("tags/v1 1/!Bug%"), "tags/v1%201/!Bug%25");
    }
}
