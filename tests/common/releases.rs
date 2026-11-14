//! A GitHub fixture addon with a run of consecutive releases (regression guard, design
//! Appendix D): `gh:o/<repo>` at `v0` and the twelve releases `v1` to `v12` after it,
//! each one zip that loads on retail and on Mists Classic, with a `core.lua` naming its
//! tag. "The next release appears" and "the newest moves back" are [`newest`]: switching
//! the tag `releases/latest` answers, with the release memo aged so the next check asks.
//!
//! Synthetic, written into the home's own fixtures by the same builders every suite uses
//! (`gh_release_v`, `gh_latest`), rather than a directory under `tests/fixtures/http/`,
//! which holds recordings of live servers.

use super::addons::toc;
use super::binding::{MAINLINE_AND_MISTS, age_release_memo};
use super::home::Home;
use super::http::{gh_latest, gh_release_v};

/// How many releases follow the first, `v0`.
pub const RUN: usize = 12;

/// The tag of release `n`: `v0` is the first, `v1` to `v12` follow it.
pub fn tag(n: usize) -> String {
    format!("v{n}")
}

/// `https://github.com/o/<repo>`.
pub fn base(repo: &str) -> String {
    format!("https://github.com/o/{repo}")
}

/// Publishes every release of `name` (`v0` to `v12`) in the home's current fixtures, as
/// `gh:o/<repo>`, with `releases/latest` at `latest`.
pub fn publish_run(home: &Home, repo: &str, name: &str, latest: &str) {
    let base = base(repo);
    for n in 0..=RUN {
        let tag = tag(n);
        let toc = toc(
            "120105, 50504",
            &[&format!("## Title: {name}"), &format!("## Version: {tag}")],
        );
        let lua = format!("-- {name} {tag}\n");
        let (toc_path, lua_path) = (format!("{name}/{name}.toc"), format!("{name}/core.lua"));
        let file = format!("{name}.zip");
        gh_release_v(
            &home.http,
            &base,
            &tag,
            &[(
                file.as_str(),
                MAINLINE_AND_MISTS,
                vec![
                    (toc_path.as_str(), toc.as_str()),
                    (lua_path.as_str(), lua.as_str()),
                ],
            )],
        );
    }
    gh_latest(&home.http, &base, latest);
}

/// `gh:o/<repo>`'s newest release becomes `tag`, and the release memo is aged by a day,
/// so the next `outdated` or `update` asks GitHub again and sees it.
pub fn newest(home: &Home, repo: &str, tag: &str) {
    gh_latest(&home.http, &base(repo), tag);
    age_release_memo(home, jiff::SignedDuration::from_hours(24));
}
