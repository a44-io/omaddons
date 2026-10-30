//! Release landings (D7): one addon per source whose newest release a test moves forward
//! between two runs of the binary. Replay restarts every fixture sequence in each process
//! (`http::fixture_seq`), so "the next probe answers a newer release" is a second scenario:
//! `land` copies the home's current scenario (`Home::next_scenario`) and serves release
//! n + 1 there as the newest; the next run of the binary replays it.

use serde_json::Value;

use super::addons::toc;
use super::archives::zip_text;
use super::home::Home;
use super::http::{fixture, fixture_bytes, gh_latest, gh_release_v};
use super::tukui::{elvui, elvui_zip, entry, etag, tukui_addon, tukui_download, tukui_list};
use super::wowace::{TAGS, moved, retagged_tree_with, serve, serve_recorded};
use super::wowi::{details, wowi_details, wowi_file};

/// The repository the GitHub and git landings serve.
const FOO: &str = "https://github.com/o/foo";
/// The WoWInterface project the WoWInterface landing serves.
const WOWI_UID: u64 = 7001;
/// The revision the recording's `v12.1.1` tag is at; release n ≥ 2 re-tags it at
/// `WOWACE_BASE + n`.
const WOWACE_BASE: u32 = 402;

/// A source a landing serves an addon from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    GitHub,
    Git,
    Wowi,
    Tukui,
    WowAce,
}

impl Source {
    pub const ALL: [Source; 5] = [
        Source::GitHub,
        Source::Git,
        Source::Wowi,
        Source::Tukui,
        Source::WowAce,
    ];

    /// For step labels and messages.
    pub fn label(self) -> &'static str {
        match self {
            Source::GitHub => "github",
            Source::Git => "git",
            Source::Wowi => "wowi",
            Source::Tukui => "tukui",
            Source::WowAce => "wowace",
        }
    }
}

/// One addon of `source`, whose releases 1 to `newest` are served.
pub struct Landing {
    source: Source,
    newest: u32,
}

impl Landing {
    /// Serves release 1 as the newest, in a fresh copy of the home's scenario.
    pub fn start(home: &mut Home, source: Source) -> Self {
        let mut landing = Self { source, newest: 0 };
        landing.land(home);
        landing
    }

    /// Serves release `newest + 1` as the newest, in a fresh copy; returns its version.
    pub fn land(&mut self, home: &mut Home) -> String {
        let n = self.newest + 1;
        assert!(
            n <= 9,
            "the {} landing serves releases 1 to 9 only",
            self.source.label()
        );
        let dir = home.next_scenario();
        match self.source {
            Source::GitHub => {
                let toc = format!("## Interface: 120105, 50504\n## Version: v{n}\n");
                let core = format!("-- foo release {n}\n");
                let flavors: &[(&str, u32)] = &[("mainline", 120_105), ("mists", 50_504)];
                let entries = vec![("Foo/Foo.toc", toc.as_str()), ("Foo/core.lua", &core)];
                let tag = format!("v{n}");
                gh_release_v(&dir, FOO, &tag, &[("Foo.zip", flavors, entries)]);
                gh_latest(&dir, FOO, &tag);
            }
            Source::Git => {
                let head = head(n);
                let atom = format!(
                    "<?xml version=\"1.0\"?><feed><entry><id>tag:github.com,2008:Grit::Commit/{head}</id><title>x</title></entry></feed>"
                );
                fixture(
                    &dir,
                    "foo-main",
                    &format!("{FOO}/commits/main.atom"),
                    200,
                    &[],
                    Some(&atom),
                );
                let core = format!("-- foo head {n}\n");
                let zip = zip_text(&[
                    ("foo-main/Foo/Foo.toc", "## Interface: 120105, 50504\n"),
                    ("foo-main/Foo/core.lua", &core),
                ]);
                fixture_bytes(
                    &dir,
                    &format!("foo-zip-{head}"),
                    &format!("https://codeload.github.com/o/foo/zip/{head}"),
                    200,
                    &[],
                    Some(&zip),
                );
            }
            Source::Wowi => {
                let version = format!("1.0.{n}");
                let toc = toc(
                    "120105, 50504",
                    &[
                        "## Title: Foo",
                        &format!("## X-WoWI-ID: {WOWI_UID}"),
                        &format!("## Version: {version}"),
                    ],
                );
                let core = format!("-- foo {version}");
                let zip = zip_text(&[("Foo/Foo.toc", &toc), ("Foo/core.lua", &core)]);
                let md5 = wowi_file(&dir, WOWI_UID, &zip);
                let entry = details(WOWI_UID, "Foo", &version, &md5);
                wowi_details(&dir, &[WOWI_UID], &[entry]);
            }
            Source::Tukui => {
                let version = format!("16.0{n}");
                let elvui = elvui();
                let words = |key: &str| -> Vec<String> {
                    elvui[key]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                };
                let (patch, dirs) = (words("patch"), words("directories"));
                let patch: Vec<&str> = patch.iter().map(String::as_str).collect();
                let dirs: Vec<&str> = dirs.iter().map(String::as_str).collect();
                let listed = entry(-2, "elvui", "ElvUI", &version, &patch, &dirs);
                tukui_list(&dir, std::slice::from_ref(&listed));
                let (first, second) = (format!("LAND{n}-A"), format!("LAND{n}-B"));
                tukui_addon(&dir, &listed, &[&first, &second]);
                let zip = elvui_zip(&version);
                tukui_download(&dir, "elvui", &first, &zip, Some(&etag(&zip)));
            }
            Source::WowAce if n == 1 => serve_recorded(home),
            Source::WowAce => {
                let rev = WOWACE_BASE + n;
                serve(
                    &dir,
                    "PROPFIND",
                    TAGS,
                    207,
                    moved("v12.1.1", rev).as_bytes(),
                );
                retagged_tree_with(&dir, rev, &format!("-- re-tagged at r{rev}\n"));
            }
        }
        self.newest = n;
        self.version(n)
    }

    pub fn source(&self) -> Source {
        self.source
    }

    /// The newest release served.
    pub fn newest(&self) -> u32 {
        self.newest
    }

    /// The ref that installs the addon.
    pub fn r#ref(&self) -> &'static str {
        match self.source {
            Source::GitHub => "gh:o/foo",
            Source::Git => "git:https://github.com/o/foo#main",
            Source::Wowi => "wowi:7001",
            Source::Tukui => "tukui:elvui",
            Source::WowAce => "wowace:bug-grabber",
        }
    }

    /// Release `n`'s version as a record and an item name it: a WowAce re-tag keeps
    /// the tag (`revision` tells them apart).
    pub fn version(&self, n: u32) -> String {
        match self.source {
            Source::GitHub => format!("v{n}"),
            Source::Git => head(n)[..7].to_owned(),
            Source::Wowi => format!("1.0.{n}"),
            Source::Tukui => format!("16.0{n}"),
            Source::WowAce => "v12.1.1".to_owned(),
        }
    }

    /// Release `n`'s source revision: WowAce's, which a re-tag moves; `None` elsewhere.
    pub fn revision(&self, n: u32) -> Option<u32> {
        (self.source == Source::WowAce).then_some(WOWACE_BASE + n)
    }

    /// A file under `Interface/AddOns` that only release `n` has, with its content
    /// (WowAce's release 1 is the recording, which has none of its own).
    pub fn marker(&self, n: u32) -> Option<(String, String)> {
        let (path, content) = match self.source {
            Source::GitHub => ("Foo/core.lua", format!("-- foo release {n}\n")),
            Source::Git => ("Foo/core.lua", format!("-- foo head {n}\n")),
            Source::Wowi => ("Foo/core.lua", format!("-- foo 1.0.{n}")),
            Source::Tukui => ("ElvUI/core.lua", format!("-- ElvUI 16.0{n}")),
            Source::WowAce if n == 1 => return None,
            Source::WowAce => (
                "!BugGrabber/BugGrabber.lua",
                format!("-- re-tagged at r{}\n", WOWACE_BASE + n),
            ),
        };
        Some((path.to_owned(), content))
    }

    /// The `net.log` purpose of this source's revalidation request.
    pub fn probe(&self) -> &'static str {
        match self.source {
            Source::GitHub => "release-latest",
            Source::Git => "commits-atom",
            Source::Wowi => "wowi-details",
            Source::Tukui => "tukui-list",
            Source::WowAce => "wowace-tags",
        }
    }

    /// The prefix every AddOns folder of the addon starts with.
    pub fn folder_prefix(&self) -> &'static str {
        match self.source {
            Source::GitHub | Source::Git | Source::Wowi => "Foo",
            Source::Tukui => "ElvUI",
            Source::WowAce => "!BugGrabber",
        }
    }
}

/// The git landing's head for release `n`: the digit `n`, 40 times.
fn head(n: u32) -> String {
    n.to_string().repeat(40)
}
