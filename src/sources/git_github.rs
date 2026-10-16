//! `git:https://github.com/<o>/<r>[.git][#<rev>]` without the `git` binary and without
//! REST (vision §5.4): a branch's head comes from `commits/<branch>.atom`, the archive
//! from `codeload.github.com`, pinned to the resolved commit so the plan installs exactly
//! what it resolved even if the branch moves.
//!
//! Revs: 40 hex characters are a commit, used as is (no request). Any other rev goes
//! through `commits/<rev>.atom`, which GitHub serves for branches and tags alike, so
//! both resolve to a commit (a branch and a tag sharing a name resolve as GitHub picks).
//! A 404 there makes the rev a tag fetched by name, for refs the feed does not serve;
//! a rev that names nothing then fails at the download. No rev is the default branch,
//! from `commits.atom`. Verified live 2026-10-15: `commits.atom` tracks the default
//! branch (`master` for shagu/pfUI, `main` for fondlez/GroupBulletinBoard), the feed of
//! tag v1.2.3 heads at that tag's commit, and a missing branch answers 404.
//! `--version` is a rev too; it must agree with `#<rev>`.

use super::changelog::{Changelog, ChangelogEntry, same_version};
use super::github::encode_segment;
use super::{Wanted, atom};
use crate::error::{Error, ErrorCode, Result, Suggestion};
use crate::model::{Asset, AssetLocation, Channel, Ref, Release, ReleaseSet};
use crate::net::{Http, Request};

/// What a rev resolved to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolved {
    /// A branch or tag (or the default branch) at the commit its feed names.
    Head { sha: String },
    /// A rev the commits feed did not serve, fetched as a tag by name.
    Tag(String),
    /// A full commit sha given directly.
    Sha(String),
}

/// One fetch of a GitHub git ref.
pub fn fetch(w: &Wanted, http: &dyn Http) -> Result<ReleaseSet> {
    let Ref::Git { url, rev } = &w.r#ref else {
        return Err(Error::internal(format!("{} is not a git ref", w.r#ref)));
    };
    let (owner, repo) = repository(url).ok_or_else(|| {
        Error::new(
            ErrorCode::RefInvalid,
            format!("{url} is not a GitHub repository URL (https://github.com/<owner>/<repo>)"),
            Suggestion::command(["omaddons", "help", "install"]),
        )
    })?;
    if let (Some(rev), Some(version)) = (rev, &w.version)
        && rev != version
    {
        return Err(Error::new(
            ErrorCode::Usage,
            format!(
                "--version {version} disagrees with the rev {rev} that {} names",
                w.r#ref
            ),
            Suggestion::command(["omaddons", "help", "install"]),
        ));
    }
    let rev = w.version.as_ref().or(rev.as_ref());
    let resolved = match rev {
        Some(rev) if is_sha(rev) => Resolved::Sha(rev.to_ascii_lowercase()),
        Some(rev) => match head(owner, repo, Some(rev), http)? {
            Some(sha) => Resolved::Head { sha },
            None => Resolved::Tag(rev.clone()),
        },
        None => match head(owner, repo, None, http)? {
            Some(sha) => Resolved::Head { sha },
            None => {
                return Err(Error::new(
                    ErrorCode::NotFound,
                    format!(
                        "{} does not exist on GitHub, or it is private (private repositories are not supported)",
                        w.r#ref
                    ),
                    Suggestion::command(["omaddons", "help", "install"]),
                ));
            }
        },
    };
    Ok(release_set(w, url, owner, repo, &resolved))
}

/// `owner` and `repo` of `https://github.com/<owner>/<repo>[.git][/]`.
pub fn repository(url: &str) -> Option<(&str, &str)> {
    let path = url
        .strip_prefix("https://github.com/")?
        .trim_end_matches('/');
    let (owner, repo) = path.split_once('/')?;
    let repo = repo.strip_suffix(".git").unwrap_or(repo);
    let plain = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    (plain(owner) && plain(repo) && !repo.contains('/')).then_some((owner, repo))
}

/// A full commit sha.
pub fn is_sha(rev: &str) -> bool {
    rev.len() == 40 && rev.chars().all(|c| c.is_ascii_hexdigit())
}

/// The commits feed of `rev` (the default branch when `None`).
fn commits_feed(owner: &str, repo: &str, rev: Option<&str>) -> String {
    match rev {
        Some(rev) => format!(
            "https://github.com/{owner}/{repo}/commits/{}.atom",
            encode_path(rev)
        ),
        None => format!("https://github.com/{owner}/{repo}/commits.atom"),
    }
}

/// The commits after `since` (an installed sha) on `rev`, newest first, at most `limit`:
/// one entry per commit, its 7-character sha and its title. One conditional request.
pub fn changelog(
    owner: &str,
    repo: &str,
    rev: Option<&str>,
    since: Option<&str>,
    limit: usize,
    http: &dyn Http,
) -> Result<Changelog> {
    let mut req = Request::get(commits_feed(owner, repo, rev), "commits-atom");
    req.validators = true;
    let body = http.send(req)?.error_for_status("commits atom")?.bytes()?;
    let mut entries = Vec::new();
    let mut reached = since.is_none();
    let mut cut = false;
    for e in atom::parse(&String::from_utf8_lossy(&body)) {
        let Some(sha) = commit_sha(&e.id).or_else(|| e.link.as_deref().and_then(commit_sha)) else {
            continue;
        };
        if since.is_some_and(|s| same_version(&sha, s)) {
            reached = true;
            break;
        }
        if entries.len() == limit {
            cut = true;
            break;
        }
        entries.push(ChangelogEntry {
            version: sha.chars().take(7).collect(),
            published_at: e.updated.and_then(|u| u.parse().ok()),
            text: e.title,
        });
    }
    let page = rev.map_or_else(
        || "commits".to_owned(),
        |r| format!("commits/{}", encode_path(r)),
    );
    Ok(Changelog {
        source: super::SourceId::Git,
        entries,
        truncated: since.is_some() && (cut || !reached),
        more_url: Some(format!("https://github.com/{owner}/{repo}/{page}")),
    })
}

/// The head commit of `rev` (the default branch when `None`); `None` on a 404.
fn head(owner: &str, repo: &str, rev: Option<&str>, http: &dyn Http) -> Result<Option<String>> {
    let url = commits_feed(owner, repo, rev);
    // Conditional: an unchanged branch whose feed carries an ETag comes back as a 304,
    // answered from the HTTP cache.
    let mut req = Request::get(url, "commits-atom");
    req.validators = true;
    let resp = http.send(req)?;
    if resp.status == 404 {
        return Ok(None);
    }
    let body = resp.error_for_status("commits atom")?.bytes()?;
    let sha = atom::parse(&String::from_utf8_lossy(&body))
        .into_iter()
        .find_map(|e| commit_sha(&e.id).or_else(|| e.link.as_deref().and_then(commit_sha)));
    sha.map(Some).ok_or_else(|| {
        Error::new(
            ErrorCode::NotFound,
            format!(
                "{owner}/{repo}: the commits feed of {} lists no commit",
                rev.unwrap_or("the default branch")
            ),
            Suggestion::text("check the branch or tag name, or give a commit sha after #"),
        )
    })
}

/// The commit sha in an Atom entry id (`tag:github.com,2008:Grit::Commit/<sha>`) or link
/// (`…/commit/<sha>`).
pub fn commit_sha(s: &str) -> Option<String> {
    let tail = s
        .split_once("Grit::Commit/")
        .or_else(|| s.split_once("/commit/"))
        .map(|(_, t)| t)?;
    let sha: String = tail.chars().take_while(char::is_ascii_hexdigit).collect();
    is_sha(&sha).then(|| sha.to_ascii_lowercase())
}

/// A rev's path segments, each percent-encoded (a tag or branch may contain `/`).
fn encode_path(rev: &str) -> String {
    rev.split('/')
        .map(encode_segment)
        .collect::<Vec<_>>()
        .join("/")
}

/// The codeload archive for a resolved rev.
pub fn codeload_url(owner: &str, repo: &str, resolved: &Resolved) -> String {
    let at = match resolved {
        Resolved::Head { sha } | Resolved::Sha(sha) => sha.clone(),
        Resolved::Tag(tag) => format!("refs/tags/{}", encode_path(tag)),
    };
    format!("https://codeload.github.com/{owner}/{repo}/zip/{at}")
}

/// One stable release with one codeload asset.
fn release_set(w: &Wanted, url: &str, owner: &str, repo: &str, resolved: &Resolved) -> ReleaseSet {
    let (identity, version) = match resolved {
        Resolved::Head { sha } | Resolved::Sha(sha) => (sha.clone(), sha.chars().take(7).collect()),
        Resolved::Tag(tag) => (tag.clone(), tag.clone()),
    };
    let filename = format!("{repo}-{}.zip", version.replace('/', "-"));
    ReleaseSet {
        r#ref: w.r#ref.clone(),
        releases: vec![Release {
            release_id: format!("git:{url}@{identity}"),
            version,
            channel: Channel::Stable,
            published_at: None,
            assets: vec![Asset {
                filename,
                location: AssetLocation::Url(codeload_url(owner, repo, resolved)),
                size: None,
                expected_hash: None,
                flavors: Vec::new(),
                name_flavors: Vec::new(),
                nolib: false,
            }],
            changelog_url: None,
            notes: None,
            name_hint: None,
        }],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA: &str = "b2f6df84a93a4ce6adbe1fd8f0372454795151f1";

    #[test]
    fn commit_shas_from_ids_and_links() {
        assert_eq!(
            commit_sha(&format!("tag:github.com,2008:Grit::Commit/{SHA}")).as_deref(),
            Some(SHA)
        );
        assert_eq!(
            commit_sha(&format!("https://github.com/shagu/pfUI/commit/{SHA}")).as_deref(),
            Some(SHA)
        );
        assert_eq!(commit_sha("tag:github.com,2008:Grit::Commit/abc"), None);
        assert_eq!(commit_sha("nothing here"), None);
    }

    #[test]
    fn revs_and_repositories() {
        assert!(is_sha(SHA) && !is_sha("master") && !is_sha(&SHA[..39]));
        assert_eq!(
            repository("https://github.com/shagu/pfUI"),
            Some(("shagu", "pfUI"))
        );
        assert_eq!(repository("https://github.com/o/r.git/"), Some(("o", "r")));
        assert_eq!(repository("https://github.com/o"), None);
        assert_eq!(repository("https://github.com/o/r/tree/main"), None);
        assert_eq!(repository("https://gitlab.com/o/r"), None);
    }

    #[test]
    fn codeload_urls_and_release_ids() {
        let w = Wanted::stable("git:https://github.com/o/r#release/1.2".parse().unwrap());
        assert_eq!(
            codeload_url("o", "r", &Resolved::Tag("release/1.2".into())),
            "https://codeload.github.com/o/r/zip/refs/tags/release/1.2"
        );
        assert_eq!(
            codeload_url("o", "r", &Resolved::Head { sha: SHA.into() }),
            format!("https://codeload.github.com/o/r/zip/{SHA}")
        );
        let set = release_set(
            &w,
            "https://github.com/o/r",
            "o",
            "r",
            &Resolved::Head { sha: SHA.into() },
        );
        let release = &set.releases[0];
        assert_eq!(
            release.release_id,
            format!("git:https://github.com/o/r@{SHA}")
        );
        assert_eq!(
            (release.version.as_str(), release.assets.len()),
            ("b2f6df8", 1)
        );
        assert_eq!(release.assets[0].filename, "r-b2f6df8.zip");
        let tagged = release_set(
            &w,
            "https://github.com/o/r",
            "o",
            "r",
            &Resolved::Tag("release/1.2".into()),
        );
        assert_eq!(tagged.releases[0].version, "release/1.2");
        assert_eq!(tagged.releases[0].assets[0].filename, "r-release-1.2.zip");
    }
}
