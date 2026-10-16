//! `url:` refs (vision §5.4): a direct https download. The host must be on the hard
//! allowlist (vision §5.7: only git hosts are user-configurable), checked here so the
//! refusal comes at plan time. `fetch` downloads nothing: the URL is the release's
//! identity, and the engine downloads the asset into the blob cache, where its hash
//! shows whether the content changed. The version is left for the TOC.

use super::{Source, SourceId, Wanted};
use crate::error::{Error, ErrorCode, Result, Suggestion};
use crate::model::{Asset, AssetLocation, Channel, Ref, Release, ReleaseSet};
use crate::net::{Allowlist, Http};

/// The `url:` source.
#[derive(Debug)]
pub struct UrlSource {
    allowlist: Allowlist,
}

impl Default for UrlSource {
    fn default() -> Self {
        Self::new()
    }
}

impl UrlSource {
    /// The hard allowlist: no CurseForge, no Wago, no user git hosts.
    pub fn new() -> Self {
        Self {
            allowlist: Allowlist::new(false, false, &[]),
        }
    }
}

impl Source for UrlSource {
    fn id(&self) -> SourceId {
        SourceId::Url
    }

    fn fetch(&self, wanted: &[Wanted], _: &dyn Http) -> Vec<Result<ReleaseSet>> {
        wanted.iter().map(|w| self.release_set(&w.r#ref)).collect()
    }
}

impl UrlSource {
    fn release_set(&self, r: &Ref) -> Result<ReleaseSet> {
        let Ref::Url { url } = r else {
            return Err(Error::internal(format!("the url source got {r}")));
        };
        let rest = url.strip_prefix("https://").unwrap_or(url);
        let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
        let host = authority
            .rsplit('@')
            .next()
            .unwrap_or(authority)
            .split(':')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        if !self.allowlist.permits(&host) {
            return Err(Error::new(
                ErrorCode::HostBlocked,
                format!("{host} is not a host omaddons downloads from"),
                Suggestion::text("download the file in a browser, then: omaddons install <path>"),
            )
            .details(serde_json::json!({ "host": host })));
        }
        let path = rest.split(['?', '#']).next().unwrap_or(rest);
        let filename = path.rsplit('/').next().unwrap_or_default().to_owned();
        Ok(ReleaseSet {
            r#ref: r.clone(),
            releases: vec![Release {
                release_id: r.to_string(),
                version: String::new(),
                channel: Channel::Stable,
                published_at: None,
                assets: vec![Asset {
                    filename,
                    location: AssetLocation::Url(url.clone()),
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
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fetch(url: &str) -> Result<ReleaseSet> {
        UrlSource::new().release_set(&format!("url:{url}").parse().unwrap())
    }

    #[test]
    fn allowlisted_hosts_only() {
        let set = fetch("https://github.com/o/r/releases/download/v1/Addon-1.0.zip?x=1").unwrap();
        let release = &set.releases[0];
        assert_eq!(
            release.release_id,
            "url:https://github.com/o/r/releases/download/v1/Addon-1.0.zip?x=1"
        );
        assert_eq!(
            (
                release.version.as_str(),
                release.assets[0].filename.as_str()
            ),
            ("", "Addon-1.0.zip")
        );
        assert!(
            matches!(&release.assets[0].location, AssetLocation::Url(u) if u.ends_with("?x=1"))
        );
        assert!(fetch("https://objects.githubusercontent.com/x/y.zip").is_ok());

        let err = fetch("https://example.org/Addon.zip").unwrap_err();
        assert_eq!((err.code, err.exit().code()), (ErrorCode::HostBlocked, 4));
        assert_eq!(
            (err.fix.display(), err.fix.argv()),
            (
                "download the file in a browser, then: omaddons install <path>",
                None
            )
        );
        assert_eq!(err.details["host"], "example.org");
        assert!(
            fetch("https://addons.wago.io/x.zip").is_err(),
            "Wago is off in M1"
        );
    }
}
