//! `git:<url>[#<tag|branch|sha>]` refs (vision §5.4), dispatched by host: GitHub
//! repositories resolve in process through the commits Atom feed and codeload
//! (`git_github`); other hosts go through the `git` binary (`git_hosts`), which the
//! command layer provides with [`super::SourceRegistry::with_git`].

use std::sync::Arc;

use super::git_hosts::GitRunner;
use super::{Source, SourceId, Wanted, git_github};
use crate::error::{Error, ErrorCode, Result, Suggestion};
use crate::model::{Ref, ReleaseSet};
use crate::net::Http;

/// The `git:` source.
#[derive(Debug, Default)]
pub struct GitSource {
    /// Runs git for non-GitHub hosts; without one they are `source_disabled`.
    pub runner: Option<Arc<GitRunner>>,
}

impl Source for GitSource {
    fn id(&self) -> SourceId {
        SourceId::Git
    }

    fn fetch(&self, wanted: &[Wanted], http: &dyn Http) -> Vec<Result<ReleaseSet>> {
        wanted.iter().map(|w| self.fetch_one(w, http)).collect()
    }
}

impl GitSource {
    fn fetch_one(&self, w: &Wanted, http: &dyn Http) -> Result<ReleaseSet> {
        let Ref::Git { url, .. } = &w.r#ref else {
            return Err(Error::internal(format!("{} is not a git ref", w.r#ref)));
        };
        match (host(url), &self.runner) {
            (Some(host), _) if host.eq_ignore_ascii_case("github.com") => {
                git_github::fetch(w, http)
            }
            (_, Some(runner)) => runner.fetch(w),
            (host, None) => Err(Error::new(
                ErrorCode::SourceDisabled,
                format!(
                    "git refs to {} need the git binary, which this command does not run",
                    host.unwrap_or("this host")
                ),
                Suggestion::command(["omaddons", "help", "install"]),
            )),
        }
    }
}

/// The host of an `https://` URL (scheme in any case).
fn host(url: &str) -> Option<&str> {
    let rest = url
        .get(..8)
        .filter(|s| s.eq_ignore_ascii_case("https://"))
        .and_then(|_| url.get(8..))?;
    rest.split(['/', '?', '#']).next().filter(|h| !h.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts() {
        assert_eq!(host("https://github.com/o/r"), Some("github.com"));
        assert_eq!(host("HTTPS://GitHub.com/o/r"), Some("GitHub.com"));
        assert_eq!(host("https://gitlab.com/o/r.git"), Some("gitlab.com"));
        assert_eq!(host("http://github.com/o/r"), None);
    }

    #[test]
    fn other_hosts_need_a_runner() {
        struct NoCalls;
        impl Http for NoCalls {
            fn send(&self, req: crate::net::Request) -> Result<crate::net::Response> {
                Err(Error::internal(format!(
                    "unexpected request to {}",
                    req.url
                )))
            }
        }
        let w = Wanted::stable("git:https://gitlab.com/o/r".parse().unwrap());
        let err = GitSource::default()
            .fetch(&[w], &NoCalls)
            .remove(0)
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::SourceDisabled);
        assert!(err.message.contains("gitlab.com"), "{}", err.message);
    }
}
