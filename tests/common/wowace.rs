//! WowAce fixtures over `tests/fixtures/http/wowace-bug-grabber/`, a recording of the
//! live server (BugGrabber's tag list, `v12.1.1` at revision 403): the recording copied
//! into a home, single responses served over it, the tag list with one tag moved, and
//! v12.1.1's tree re-tagged at another revision.

use std::fs;
use std::path::Path;

use serde_json::{Value, json};

use super::home::Home;
use super::http::scenario;

/// BugGrabber's tag list, which a `PROPFIND` reads.
pub const TAGS: &str = "https://repos.wowace.com/wow/bug-grabber/tags/";

/// The recorded scenario, copied into the home's fixtures, where a test edits it.
pub fn serve_recorded(home: &Home) {
    let from = scenario("wowace-bug-grabber").join("repos.wowace.com");
    let to = home.http.join("repos.wowace.com");
    fs::create_dir_all(&to).unwrap();
    for item in fs::read_dir(from).unwrap() {
        let item = item.unwrap();
        fs::copy(item.path(), to.join(item.file_name())).unwrap();
    }
}

/// Serves `body` with `status` for `method url` in `dir`, in place of the recorded
/// fixture for it when there is one (no key may have two).
pub fn serve(dir: &Path, method: &str, url: &str, status: u16, body: &[u8]) {
    let host = dir.join("repos.wowace.com");
    fs::create_dir_all(&host).unwrap();
    let recorded = fs::read_dir(&host).unwrap().find_map(|item| {
        let path = item.unwrap().path();
        let text = fs::read_to_string(&path).ok()?;
        let doc: Value = serde_json::from_str(&text).ok()?;
        let same = doc["request"]["method"] == method && doc["request"]["url"] == url;
        same.then(|| path.file_stem().unwrap().to_string_lossy().into_owned())
    });
    let n = fs::read_dir(&host).unwrap().count();
    let stem = recorded.unwrap_or_else(|| format!("edited-{n}"));
    fs::write(host.join(format!("{stem}.body")), body).unwrap();
    let doc = json!({
        "schema_version": "1.0",
        "request": {"method": method, "url": url},
        "responses": [{"status": status, "headers": [], "body_file": format!("{stem}.body")}],
    });
    fs::write(host.join(format!("{stem}.json")), doc.to_string()).unwrap();
}

/// The recorded tag list with `tag`'s `version-name` moved to `to`: a re-tag
/// upstream, or (another tag) a commit that is no update here.
pub fn moved(tag: &str, to: u32) -> String {
    let dir = scenario("wowace-bug-grabber").join("repos.wowace.com");
    let path = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| {
            let name = p.file_name().unwrap().to_string_lossy();
            name.starts_with("propfind-") && name.ends_with(".body")
        })
        .unwrap();
    let body = fs::read_to_string(path).unwrap();
    let at = body.find(&format!("/tags/{tag}/</D:href>")).unwrap();
    let (head, tail) = body.split_at(at);
    let open = tail.find("version-name>").unwrap() + "version-name>".len();
    let close = open + tail[open..].find('<').unwrap();
    format!("{head}{}{to}{}", &tail[..open], &tail[close..])
}

/// v12.1.1's tree as of revision `rev`, a re-tag whose `BugGrabber.lua` says so.
pub fn retagged_tree(dir: &Path, rev: u32) {
    retagged_tree_with(dir, rev, "-- re-tagged\n");
}

/// v12.1.1's tree as of revision `rev`, a re-tag whose `BugGrabber.lua` is `lua`.
pub fn retagged_tree_with(dir: &Path, rev: u32, lua: &str) {
    let base = format!("https://repos.wowace.com/wow/bug-grabber/!svn/bc/{rev}/tags/v12.1.1/");
    let names = ["!BugGrabber.toc", ".pkgmeta", "BugGrabber.lua", "load.xml"];
    serve(dir, "GET", &base, 200, index(&names).as_bytes());
    for (name, body) in [
        (
            "!BugGrabber.toc",
            "## Interface: 120105\n## Version: @project-version@\nload.xml\n",
        ),
        (".pkgmeta", "package-as: \"!BugGrabber\"\n"),
        ("BugGrabber.lua", lua),
        ("load.xml", "<Ui></Ui>\n"),
    ] {
        serve(dir, "GET", &format!("{base}{name}"), 200, body.as_bytes());
    }
}

/// The WowAce tag at two revisions (regression guard, design Appendix D): served in
/// `dir` over the recording, the tag list names `v12.1.1` at revision `rev`, and that
/// revision's tree has a `BugGrabber.lua` naming it. One scenario at 1390 and the next at
/// 1402 is the same tag re-tagged upstream, with other bytes.
pub fn tag_at(dir: &Path, rev: u32) {
    serve(dir, "PROPFIND", TAGS, 207, moved("v12.1.1", rev).as_bytes());
    retagged_tree_with(dir, rev, &format!("-- v12.1.1 at revision {rev}\n"));
}

/// A mod_dav_svn HTML index naming `entries` (a trailing `/` marks a folder).
pub fn index(entries: &[&str]) -> String {
    let items: String = entries
        .iter()
        .map(|e| format!("  <li><a href=\"{e}\">{e}</a></li>\n"))
        .collect();
    format!(
        "<html><head><title>bug-grabber - Revision 403: /tags/v12.1.1</title></head>\n\
         <body>\n <h2>bug-grabber - Revision 403: /tags/v12.1.1</h2>\n <ul>\n\
         \x20 <li><a href=\"../\">..</a></li>\n{items} </ul>\n</body></html>\n"
    )
}
