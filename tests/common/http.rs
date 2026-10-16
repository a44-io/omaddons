//! HTTP fixture scenarios for tests: write synthetic fixtures, and build the production
//! network stack over a replay (or, for re-recording, a record) of a scenario.

use std::fs;
use std::path::{Path, PathBuf};

use omaddons::net::{Http, NetConfig, NetLog, select_transport, stack_with};
use omaddons::state::paths::Paths;
use serde_json::json;

use super::archives::zip_text;
use super::env::TestEnv;

/// Writes a one-response fixture for a GET of `url` under `scenario/<host>/`.
pub fn fixture(
    scenario: &Path,
    name: &str,
    url: &str,
    status: u16,
    headers: &[(&str, &str)],
    body: Option<&str>,
) {
    fixture_bytes(
        scenario,
        name,
        url,
        status,
        headers,
        body.map(str::as_bytes),
    );
}

/// [`fixture`] with a binary body (a zip served as a release asset).
pub fn fixture_bytes(
    scenario: &Path,
    name: &str,
    url: &str,
    status: u16,
    headers: &[(&str, &str)],
    body: Option<&[u8]>,
) {
    let mut resp = Resp::status(status);
    for (header, value) in headers {
        resp = resp.header(header, value);
    }
    resp.body = body.map(<[u8]>::to_vec);
    fixture_seq(scenario, name, url, &[resp]);
}

/// One response of a fixture sequence.
#[derive(Debug, Clone)]
pub struct Resp {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
}

impl Resp {
    /// 200 with a body.
    pub fn ok(body: impl Into<Vec<u8>>) -> Self {
        Self {
            status: 200,
            headers: Vec::new(),
            body: Some(body.into()),
        }
    }

    /// Any status, empty body.
    pub fn status(code: u16) -> Self {
        Self {
            status: code,
            headers: Vec::new(),
            body: None,
        }
    }

    /// Adds a header (the name lowercased, as fixtures store it).
    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers
            .push((name.to_ascii_lowercase(), value.to_owned()));
        self
    }
}

/// A multi-response fixture for a GET of `url`: served in order, the last repeating.
/// Replay restarts every sequence in each process, so a sequence never spans two CLI
/// runs; scenarios spanning commands switch fixture sets with `Home::scenario`.
pub fn fixture_seq(scenario: &Path, name: &str, url: &str, responses: &[Resp]) {
    let dir = scenario.join(host_of(url));
    fs::create_dir_all(&dir).unwrap();
    let responses: Vec<serde_json::Value> = responses
        .iter()
        .enumerate()
        .map(|(i, resp)| {
            let body_file = resp.body.as_ref().map(|b| {
                let file = if i == 0 {
                    format!("{name}.body")
                } else {
                    format!("{name}.{i}.body")
                };
                fs::write(dir.join(&file), b).unwrap();
                file
            });
            json!({"status": resp.status, "headers": resp.headers, "body_file": body_file})
        })
        .collect();
    let doc = json!({
        "schema_version": "1.0",
        "request": {"method": "GET", "url": url},
        "responses": responses,
    });
    fs::write(dir.join(format!("{name}.json")), doc.to_string()).unwrap();
}

/// The host of `url` (`https://github.com/o/r` → `github.com`).
fn host_of(url: &str) -> &str {
    url.split("://")
        .nth(1)
        .and_then(|r| r.split('/').next())
        .unwrap()
}

/// `o-r` for `https://github.com/o/r`, for fixture names.
fn slug(base: &str) -> String {
    let mut parts: Vec<&str> = base.rsplit('/').take(2).collect();
    parts.reverse();
    parts.join("-")
}

/// One release asset: (file name, release.json flavors with interfaces, zip entries).
pub type Asset<'a> = (&'a str, &'a [(&'a str, u32)], Vec<(&'a str, &'a str)>);

/// Serves release `tag` of the GitHub repository at `base` (`https://github.com/o/r`)
/// as its latest: [`gh_release_v`] plus [`gh_latest`].
pub fn gh_release(scenario: &Path, base: &str, tag: &str, assets: &[Asset<'_>]) {
    gh_release_v(scenario, base, tag, assets);
    gh_latest(scenario, base, tag);
}

/// The one `releases/latest` 302 fixture of the repository at `base`, pointing at
/// `tag`. It replaces any earlier one (found by its URL, whatever its name), so several
/// tags of one repository coexist in a scenario.
pub fn gh_latest(scenario: &Path, base: &str, tag: &str) {
    let url = format!("{base}/releases/latest");
    let dir = scenario.join(host_of(base));
    if let Ok(listing) = fs::read_dir(&dir) {
        for item in listing {
            let path = item.unwrap().path();
            if path.extension().is_none_or(|e| e != "json") {
                continue;
            }
            let doc: serde_json::Value =
                serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
            if doc["request"]["url"] == url.as_str() {
                fs::remove_file(&path).unwrap();
            }
        }
    }
    fixture(
        scenario,
        &format!("latest-{}-{tag}", slug(base)),
        &url,
        302,
        &[("location", &format!("{base}/releases/tag/{tag}"))],
        None,
    );
}

/// Release `tag` of the repository at `base`: its release.json listing `assets` (empty
/// flavors leave an asset's metadata empty) and each asset as a synthetic zip, without
/// touching `releases/latest` (what installing `--version <tag>` fetches).
pub fn gh_release_v(scenario: &Path, base: &str, tag: &str, assets: &[Asset<'_>]) {
    let slug = slug(base);
    let entries: Vec<serde_json::Value> = assets
        .iter()
        .map(|(file, flavors, _)| {
            json!({
                "filename": file,
                "nolib": false,
                "metadata": flavors.iter().map(|(f, i)| json!({ "flavor": f, "interface": i })).collect::<Vec<_>>(),
            })
        })
        .collect();
    let manifest = json!({ "releases": entries }).to_string();
    fixture(
        scenario,
        &format!("manifest-{slug}-{tag}"),
        &format!("{base}/releases/download/{tag}/release.json"),
        200,
        &[],
        Some(&manifest),
    );
    for (file, _, entries) in assets {
        fixture_bytes(
            scenario,
            &format!("asset-{slug}-{tag}-{file}"),
            &format!("{base}/releases/download/{tag}/{file}"),
            200,
            &[],
            Some(&zip_text(entries)),
        );
    }
}

/// `<base>/releases.atom` with one entry per (tag, release-notes HTML), newest first,
/// shaped like GitHub's feed (the decoder's own sample in `src/sources/atom.rs`). With
/// `etag`, the first response carries that ETag and later ones are 304.
pub fn gh_feed(scenario: &Path, base: &str, entries: &[(&str, &str)], etag: Option<&str>) {
    let repo = base.rsplit('/').next().unwrap();
    let mut xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <feed xmlns=\"http://www.w3.org/2005/Atom\" xmlns:media=\"http://search.yahoo.com/mrss/\" xml:lang=\"en-US\">\n\
         \x20 <id>tag:github.com,2008:{base}/releases</id>\n\
         \x20 <link type=\"text/html\" rel=\"alternate\" href=\"{base}/releases\"/>\n\
         \x20 <link type=\"application/atom+xml\" rel=\"self\" href=\"{base}/releases.atom\"/>\n\
         \x20 <title>Release notes from {repo}</title>\n\
         \x20 <updated>{}</updated>\n",
        feed_time(0)
    );
    for (i, (tag, notes)) in entries.iter().enumerate() {
        xml.push_str(&format!(
            "  <entry>\n\
             \x20   <id>tag:github.com,2008:Repository/1/{tag}</id>\n\
             \x20   <updated>{}</updated>\n\
             \x20   <link rel=\"alternate\" type=\"text/html\" href=\"{base}/releases/tag/{}\"/>\n\
             \x20   <title>{}</title>\n\
             \x20   <content type=\"html\">{}</content>\n\
             \x20   <author>\n      <name>someone</name>\n    </author>\n\
             \x20 </entry>\n",
            feed_time(i),
            encode_tag(tag),
            escape_xml(tag),
            escape_xml(notes),
        ));
    }
    xml.push_str("</feed>\n");
    let url = format!("{base}/releases.atom");
    let name = format!("feed-{}", slug(base));
    let responses = match etag {
        Some(etag) => vec![
            Resp::ok(xml).header("etag", etag),
            Resp::status(304).header("etag", etag),
        ],
        None => vec![Resp::ok(xml)],
    };
    fixture_seq(scenario, &name, &url, &responses);
}

/// The `updated` time of the `i`th newest feed entry: a day apart, newest first.
fn feed_time(i: usize) -> String {
    format!("2026-10-{:02}T12:00:00Z", 28 - i.min(27))
}

/// A tag as GitHub writes it in a URL path (`bundle/v1+2` → `bundle%2Fv1%2B2`).
fn encode_tag(tag: &str) -> String {
    tag.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                char::from(b).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn escape_xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The env's paths.
pub fn paths(env: &TestEnv) -> Paths {
    Paths::with_roots(env.home(), env.config(), env.state(), env.cache())
}

/// The production stack over a replay of `scenario`; `net.log` goes to the env.
pub fn replay(env: &TestEnv, scenario: &Path) -> Box<dyn Http> {
    let fixtures = scenario.display().to_string();
    let lookup = move |name: &str| (name == "OMADDONS_HTTP_FIXTURES").then(|| fixtures.clone());
    let paths = paths(env);
    let transport = select_transport(&lookup, true, &paths);
    stack_with(
        &NetConfig::default(),
        NetLog::new(&paths).unwrap(),
        &paths,
        transport,
    )
}

/// The production stack recording into `scenario` (real network).
pub fn record(env: &TestEnv, scenario: &Path) -> Box<dyn Http> {
    fs::create_dir_all(scenario).unwrap();
    let fixtures = scenario.display().to_string();
    let lookup = move |name: &str| match name {
        "OMADDONS_HTTP_FIXTURES" => Some(fixtures.clone()),
        "OMADDONS_RECORD" => Some("1".to_owned()),
        _ => None,
    };
    let paths = paths(env);
    let transport = select_transport(&lookup, true, &paths);
    stack_with(
        &NetConfig::default(),
        NetLog::new(&paths).unwrap(),
        &paths,
        transport,
    )
}

/// A committed scenario under `tests/fixtures/http/`.
pub fn scenario(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/http")
        .join(name)
}
