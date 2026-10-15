//! `git:<url>[#<tag|branch|sha>]` refs (vision §5.4), dispatched by host: GitHub
//! repositories resolve in process through the commits Atom feed and codeload
//! (`git_github`); other hosts arrive with the git-hosts source and are
//! `source_disabled` until then.

use super::{Source, SourceId, Wanted, git_github};
use crate::error::{Error, ErrorCode, Result, Suggestion};
use crate::model::{Ref, ReleaseSet};
use crate::net::Http;

/// The `git:` source.
#[derive(Debug, Default)]
pub struct GitSource;

impl Source for GitSource {
    fn id(&self) -> SourceId {
        SourceId::Git
    }

    fn fetch(&self, wanted: &[Wanted], http: &dyn Http) -> Vec<Result<ReleaseSet>> {
        wanted.iter().map(|w| fetch_one(w, http)).collect()
    }
}

fn fetch_one(w: &Wanted, http: &dyn Http) -> Result<ReleaseSet> {
    let Ref::Git { url, .. } = &w.r#ref else {
        return Err(Error::internal(format!("{} is not a git ref", w.r#ref)));
    };
    match host(url) {
        Some(host) if host.eq_ignore_ascii_case("github.com") => git_github::fetch(w, http),
        host => Err(Error::new(
            ErrorCode::SourceDisabled,
            format!(
                "git refs to {} arrive with the git-hosts source; this build reads GitHub repositories only",
                host.unwrap_or("this host")
            ),
            Suggestion::command(["omaddons", "help", "install"]),
        )),
    }
}

/// The host of an `https://` URL.
fn host(url: &str) -> Option<&str> {
    let rest = url.strip_prefix("https://")?;
    rest.split(['/', '?', '#']).next().filter(|h| !h.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts() {
        assert_eq!(host("https://github.com/o/r"), Some("github.com"));
        assert_eq!(host("https://gitlab.com/o/r.git"), Some("gitlab.com"));
        assert_eq!(host("http://github.com/o/r"), None);
    }

    #[test]
    fn other_hosts_are_disabled() {
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
        let err = GitSource.fetch(&[w], &NoCalls).remove(0).unwrap_err();
        assert_eq!(err.code, ErrorCode::SourceDisabled);
        assert!(err.message.contains("gitlab.com"), "{}", err.message);
    }
}
