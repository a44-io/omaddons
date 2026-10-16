//! What users type to name an addon, turned into a canonical [`Ref`] (vision §5.4):
//! canonical refs, pasted URLs, local paths, `owner/repo` shorthand, installed addon
//! names and catalog names. An input that could mean several things fails with every
//! candidate as an exact command, never a guess. The only side effect is checking
//! whether a path exists and what it is.

use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::Serialize;

use crate::error::{Error, ErrorCode, Result, Suggestion};
use crate::model::{Ref, RefKind};

/// A parsed user ref and the hints its URL carried.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ParsedRef {
    #[serde(rename = "ref")]
    pub r#ref: Ref,
    /// A release tag from a release URL; install treats it like `--version`.
    pub version_hint: Option<String>,
    /// An asset file name from a release-download URL.
    pub asset_hint: Option<String>,
    pub matched_by: MatchKind,
}

/// How an input was recognized.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MatchKind {
    Canonical,
    Url,
    Path,
    Installed,
    Catalog,
    Shorthand,
}

/// An installed addon a bare name matched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledMatch {
    pub name: String,
    pub r#ref: Ref,
    pub target_id: String,
}

/// A catalog entry a bare name matched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogMatch {
    pub name: String,
    pub r#ref: Ref,
}

/// What the parser may consult besides the input: the working directory, configured
/// git hosts, and lookups the command layer wires to state records and the catalog.
pub struct RefContext<'a> {
    pub cwd: PathBuf,
    pub git_hosts: &'a [String],
    /// Case-insensitive on record name and folder key.
    pub installed: &'a dyn Fn(&str) -> Vec<InstalledMatch>,
    /// Case-insensitive on name and aliases.
    pub catalog: &'a dyn Fn(&str) -> Option<CatalogMatch>,
}

/// Parses user input into a canonical ref. Errors: `ref_invalid` (2), `ref_ambiguous`
/// (3), `not_found` (3), `source_disabled` (1).
pub fn parse_user_ref(input: &str, ctx: &RefContext<'_>) -> Result<ParsedRef> {
    let input = input.trim();
    if input.is_empty() {
        return Err(invalid(input, "it is empty"));
    }
    if let Some(prefix) = input.split_once(':').map(|(p, _)| p)
        && RefKind::ALL.iter().any(|k| k.prefix() == prefix)
    {
        return canonical(input, ctx);
    }
    if let Some(rest) = input.strip_prefix("https://") {
        return url(input, rest, ctx);
    }
    if input.starts_with("http://") {
        let https = input.replacen("http://", "https://", 1);
        return Err(Error::new(
            ErrorCode::RefInvalid,
            format!("{input:?} is not https; omaddons fetches over https only"),
            Suggestion::command(["omaddons", "install", &https]),
        ));
    }
    local_or_name(input, ctx)
}

fn parsed(r#ref: Ref, matched_by: MatchKind) -> ParsedRef {
    ParsedRef {
        r#ref,
        version_hint: None,
        asset_hint: None,
        matched_by,
    }
}

fn invalid(input: &str, why: &str) -> Error {
    Error::new(
        ErrorCode::RefInvalid,
        format!("{input:?} is not a valid addon ref: {why}"),
        Suggestion::command(["omaddons", "help", "install"]),
    )
}

// ---------------------------------------------------------------------------------------
// Canonical refs.

fn canonical(input: &str, ctx: &RefContext<'_>) -> Result<ParsedRef> {
    let r: Ref = input
        .parse()
        .map_err(|e: crate::model::RefParseError| invalid(input, &e.to_string()))?;
    let r = match r {
        Ref::GitHub { owner, repo } => {
            check_github(input, &owner, &repo)?;
            Ref::GitHub { owner, repo }
        }
        Ref::Tukui { slug } => {
            // Slugs compare ignoring case, as the link forms do: `tukui:ElvUI` is
            // `tukui:elvui`.
            let slug = slug.to_ascii_lowercase();
            if !is_slug(&slug) {
                return Err(invalid(input, "a tukui slug is letters, digits and '-'"));
            }
            Ref::Tukui { slug }
        }
        Ref::File { path } => Ref::File {
            path: absolute(&ctx.cwd, &path),
        },
        other => other,
    };
    Ok(parsed(r, MatchKind::Canonical))
}

/// GitHub's charset: owners `[A-Za-z0-9-]`, repos `[A-Za-z0-9._-]` but never `.`/`..`.
fn check_github(input: &str, owner: &str, repo: &str) -> Result<()> {
    let owner_ok = !owner.is_empty()
        && owner
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-');
    let repo_ok = !repo.is_empty()
        && repo != "."
        && repo != ".."
        && repo
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b));
    if owner_ok && repo_ok {
        Ok(())
    } else {
        Err(invalid(
            input,
            "a GitHub owner is letters, digits and '-'; a repo also allows '.' and '_'",
        ))
    }
}

pub(crate) fn is_github_owner(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

pub(crate) fn is_github_repo(s: &str) -> bool {
    !s.is_empty()
        && s != "."
        && s != ".."
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

pub(crate) fn is_slug(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn absolute(cwd: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}

// ---------------------------------------------------------------------------------------
// URLs.

fn url(input: &str, rest: &str, ctx: &RefContext<'_>) -> Result<ParsedRef> {
    let (without_fragment, fragment) = match rest.split_once('#') {
        Some((url, rev)) => (url, Some(rev)),
        None => (rest, None),
    };
    let (authority, path_and_query) = without_fragment
        .split_once('/')
        .unwrap_or((without_fragment, ""));
    let host = authority
        .rsplit('@')
        .next()
        .unwrap_or(authority)
        .split(':')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host).to_owned();
    let (path, query) = path_and_query
        .split_once('?')
        .unwrap_or((path_and_query, ""));
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let with_url = |r: Ref| parsed(r, MatchKind::Url);

    match host.as_str() {
        "github.com" if segments.len() >= 2 => {
            let owner = segments[0];
            let repo = segments[1].strip_suffix(".git").unwrap_or(segments[1]);
            if !is_github_owner(owner) || !is_github_repo(repo) {
                return Err(invalid(input, "the GitHub owner or repo is not valid"));
            }
            let gh = || Ref::GitHub {
                owner: owner.to_owned(),
                repo: repo.to_owned(),
            };
            return Ok(match &segments[2..] {
                ["releases", "tag", tag] => ParsedRef {
                    version_hint: Some((*tag).to_owned()),
                    ..with_url(gh())
                },
                ["releases", "download", tag, file] => ParsedRef {
                    version_hint: Some((*tag).to_owned()),
                    asset_hint: Some((*file).to_owned()),
                    ..with_url(gh())
                },
                ["tree", branch @ ..] if !branch.is_empty() => {
                    let rev = branch.join("/");
                    let git = format!("git:https://github.com/{owner}/{repo}#{rev}");
                    with_url(
                        git.parse()
                            .map_err(|_| invalid(input, "the branch is not a valid rev"))?,
                    )
                }
                _ => with_url(gh()),
            });
        }
        // The site's pages, and the CDN link `UIDownload` and browsers produce.
        "wowinterface.com" | "cdn.wowinterface.com" => {
            let id = segments.last().and_then(|page| match *page {
                "fileinfo.php" | "getfile.php" => query
                    .split('&')
                    .find_map(|kv| kv.strip_prefix("id="))?
                    .parse::<u64>()
                    .ok(),
                _ => {
                    let digits = page
                        .strip_prefix("info")
                        .or_else(|| page.strip_prefix("download"))?;
                    digits.split(['-', '.']).next()?.parse::<u64>().ok()
                }
            });
            return match id {
                Some(id) => Ok(with_url(Ref::Wowi { id })),
                None => Err(invalid(
                    input,
                    "a WoWInterface link names info<id>-<name>.html or fileinfo.php?id=<id>",
                )),
            };
        }
        "tukui.org" => {
            let slug = query
                .split('&')
                .find_map(|kv| kv.strip_prefix("ui="))
                .or_else(|| match segments.as_slice() {
                    ["addon", slug] | ["addons", slug] | [slug] if !slug.contains('.') => {
                        Some(*slug)
                    }
                    _ => None,
                })
                .map(str::to_ascii_lowercase);
            return match slug.filter(|s| is_slug(s)) {
                Some(slug) => Ok(with_url(Ref::Tukui { slug })),
                None => Err(invalid(
                    input,
                    "a Tukui link names the addon, e.g. tukui.org/addon/elvui",
                )),
            };
        }
        "curseforge.com" | "addons.wago.io" => {
            let name = if host == "curseforge.com" {
                "CurseForge"
            } else {
                "Wago"
            };
            return Err(Error::new(
                ErrorCode::SourceDisabled,
                format!("{name} links are not supported yet: {name} arrives in M5"),
                Suggestion::command(["omaddons", "help", "install"]),
            ));
        }
        _ => {}
    }
    let base = format!("https://{without_fragment}");
    let git_host = ctx.git_hosts.iter().any(|h| h.eq_ignore_ascii_case(&host));
    if git_host || path.ends_with(".git") {
        let git = match fragment {
            Some(rev) => format!("git:{base}#{rev}"),
            None => format!("git:{base}"),
        };
        return Ok(with_url(git.parse().map_err(
            |e: crate::model::RefParseError| invalid(input, &e.to_string()),
        )?));
    }
    let url = format!("url:https://{rest}");
    Ok(with_url(url.parse().map_err(
        |e: crate::model::RefParseError| invalid(input, &e.to_string()),
    )?))
}

// ---------------------------------------------------------------------------------------
// Paths, shorthand and names.

fn local_or_name(input: &str, ctx: &RefContext<'_>) -> Result<ParsedRef> {
    let explicit = input.starts_with('/') || input.starts_with("./") || input.starts_with("../");
    let path = absolute(&ctx.cwd, Path::new(input));
    let exists = std::fs::symlink_metadata(&path).is_ok();
    if explicit {
        return if exists {
            local(input, &path)
        } else {
            Err(Error::new(
                ErrorCode::NotFound,
                format!("{input} does not exist"),
                Suggestion::command(["omaddons", "help", "install"]),
            ))
        };
    }

    let shorthand = match input.split_once('/') {
        Some((owner, repo))
            if !repo.contains('/') && is_github_owner(owner) && is_github_repo(repo) =>
        {
            Some(Ref::GitHub {
                owner: owner.to_owned(),
                repo: repo.to_owned(),
            })
        }
        _ => None,
    };
    if let Some(gh) = shorthand {
        if exists {
            let file = local(input, &path)?.r#ref;
            return Err(ambiguous(
                input,
                &[(file, "a local path".to_owned()), (gh, "GitHub".to_owned())],
            ));
        }
        return Ok(parsed(gh, MatchKind::Shorthand));
    }
    if input.contains('/') {
        return if exists {
            local(input, &path)
        } else {
            Err(Error::new(
                ErrorCode::NotFound,
                format!("{input} does not exist"),
                Suggestion::command(["omaddons", "help", "install"]),
            ))
        };
    }

    // A bare name: installed addons, the catalog, and a local entry of that name.
    let mut found: Vec<(Ref, String, MatchKind)> = Vec::new();
    for m in (ctx.installed)(input) {
        found.push((
            m.r#ref,
            format!("installed as {} on {}", m.name, m.target_id),
            MatchKind::Installed,
        ));
    }
    if let Some(m) = (ctx.catalog)(input) {
        found.push((
            m.r#ref,
            format!("the catalog entry {}", m.name),
            MatchKind::Catalog,
        ));
    }
    if exists && let Ok(p) = local(input, &path) {
        found.push((p.r#ref, "a local path".to_owned(), MatchKind::Path));
    }
    let mut distinct: Vec<&Ref> = found.iter().map(|(r, _, _)| r).collect();
    distinct.sort();
    distinct.dedup();
    match (distinct.len(), found.first()) {
        (0, _) | (_, None) => Err(Error::new(
            ErrorCode::NotFound,
            format!("no installed addon or catalog entry is named {input:?}"),
            Suggestion::command(["omaddons", "help", "install"]),
        )),
        (1, Some((r, _, kind))) => Ok(parsed(r.clone(), *kind)),
        _ => {
            let mut candidates: Vec<(Ref, String)> = Vec::new();
            for (r, from, _) in found {
                if !candidates.iter().any(|(c, _)| *c == r) {
                    candidates.push((r, from));
                }
            }
            Err(ambiguous(input, &candidates))
        }
    }
}

/// A local zip file or directory as a `file:` ref.
fn local(input: &str, path: &Path) -> Result<ParsedRef> {
    let is_zip = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("zip"))
        && path.is_file();
    if is_zip || path.is_dir() {
        return Ok(parsed(
            Ref::File {
                path: path.to_path_buf(),
            },
            MatchKind::Path,
        ));
    }
    Err(invalid(
        input,
        "a local addon is a .zip file or a directory",
    ))
}

/// `ref_ambiguous` with every candidate as an exact `omaddons install` command.
fn ambiguous(input: &str, candidates: &[(Ref, String)]) -> Error {
    let install =
        |r: &Ref| Suggestion::command(["omaddons".to_owned(), "install".to_owned(), r.to_string()]);
    let Some(((first, _), rest)) = candidates.split_first() else {
        return invalid(input, "it matches nothing");
    };
    let listed: Vec<String> = candidates
        .iter()
        .map(|(r, from)| format!("{r} ({from})"))
        .collect();
    let mut error = Error::new(
        ErrorCode::RefAmbiguous,
        format!("{input:?} could mean {}: {}", candidates.len(), listed.join(", ")),
        install(first),
    )
    .details(serde_json::json!({
        "input": input,
        "candidates": candidates.iter().map(|(r, from)| serde_json::json!({ "ref": r.to_string(), "from": from })).collect::<Vec<_>>(),
    }));
    for (r, _) in rest {
        error = error.alternative(install(r));
    }
    error
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn gh(owner: &str, repo: &str) -> Ref {
        Ref::GitHub {
            owner: owner.into(),
            repo: repo.into(),
        }
    }

    fn with_ctx<T>(cwd: &Path, f: impl FnOnce(&RefContext<'_>) -> T) -> T {
        let installed = |name: &str| -> Vec<InstalledMatch> {
            match name.to_ascii_lowercase().as_str() {
                "weakauras" => vec![InstalledMatch {
                    name: "WeakAuras".into(),
                    r#ref: gh("WeakAuras", "WeakAuras2"),
                    target_id: "retail".into(),
                }],
                "details" => vec![InstalledMatch {
                    name: "Details".into(),
                    r#ref: gh("Tercioo", "Details-Damage-Meter"),
                    target_id: "retail".into(),
                }],
                _ => Vec::new(),
            }
        };
        let catalog = |name: &str| -> Option<CatalogMatch> {
            match name.to_ascii_lowercase().as_str() {
                "weakauras" => Some(CatalogMatch {
                    name: "weakauras".into(),
                    r#ref: gh("weakauras", "weakauras2"),
                }),
                "details" => Some(CatalogMatch {
                    name: "details".into(),
                    r#ref: Ref::Wowi { id: 23_534 },
                }),
                "elvui" => Some(CatalogMatch {
                    name: "elvui".into(),
                    r#ref: Ref::Tukui {
                        slug: "elvui".into(),
                    },
                }),
                _ => None,
            }
        };
        let hosts = vec!["codeberg.org".to_owned()];
        let ctx = RefContext {
            cwd: cwd.to_path_buf(),
            git_hosts: &hosts,
            installed: &installed,
            catalog: &catalog,
        };
        f(&ctx)
    }

    /// (input, canonical ref or error code, matched_by, version hint, asset hint)
    type Case = (
        &'static str,
        &'static str,
        Option<MatchKind>,
        Option<&'static str>,
        Option<&'static str>,
    );

    #[rustfmt::skip]
    const CASES: &[Case] = &[
        ("gh:WeakAuras/WeakAuras2", "gh:WeakAuras/WeakAuras2", Some(MatchKind::Canonical), None, None),
        ("  gh:a-b/c.d_e  ", "gh:a-b/c.d_e", Some(MatchKind::Canonical), None, None),
        ("gh:a_b/c", "ref_invalid", None, None, None),
        ("gh:a/..", "ref_invalid", None, None, None),
        ("gh:a", "ref_invalid", None, None, None),
        ("wowi:5108", "wowi:5108", Some(MatchKind::Canonical), None, None),
        ("wowi:abc", "ref_invalid", None, None, None),
        ("cf:61284", "cf:61284", Some(MatchKind::Canonical), None, None),
        ("tukui:elvui", "tukui:elvui", Some(MatchKind::Canonical), None, None),
        ("tukui:ElvUI", "tukui:elvui", Some(MatchKind::Canonical), None, None),
        ("tukui:elv_ui", "ref_invalid", None, None, None),
        ("wago:abcd", "wago:abcd", Some(MatchKind::Canonical), None, None),
        ("git:https://codeberg.org/a/b.git#main", "git:https://codeberg.org/a/b.git#main", Some(MatchKind::Canonical), None, None),
        ("git:https://codeberg.org/a/b.git#", "ref_invalid", None, None, None),
        ("git:http://codeberg.org/a/b.git", "ref_invalid", None, None, None),
        ("url:https://example.org/a.zip", "url:https://example.org/a.zip", Some(MatchKind::Canonical), None, None),
        ("url:http://example.org/a.zip", "ref_invalid", None, None, None),
        ("https://github.com/WeakAuras/WeakAuras2", "gh:WeakAuras/WeakAuras2", Some(MatchKind::Url), None, None),
        ("https://www.github.com/WeakAuras/WeakAuras2/", "gh:WeakAuras/WeakAuras2", Some(MatchKind::Url), None, None),
        ("https://github.com/WeakAuras/WeakAuras2.git", "gh:WeakAuras/WeakAuras2", Some(MatchKind::Url), None, None),
        ("https://github.com/WeakAuras/WeakAuras2/releases/tag/5.19.10", "gh:WeakAuras/WeakAuras2", Some(MatchKind::Url), Some("5.19.10"), None),
        ("https://github.com/WeakAuras/WeakAuras2/releases/download/5.19.10/WeakAuras-5.19.10.zip", "gh:WeakAuras/WeakAuras2", Some(MatchKind::Url), Some("5.19.10"), Some("WeakAuras-5.19.10.zip")),
        ("https://github.com/WeakAuras/WeakAuras2/tree/main", "git:https://github.com/WeakAuras/WeakAuras2#main", Some(MatchKind::Url), None, None),
        ("https://github.com/a/b/tree/feature/x", "git:https://github.com/a/b#feature/x", Some(MatchKind::Url), None, None),
        ("https://github.com/WeakAuras/WeakAuras2/issues/12", "gh:WeakAuras/WeakAuras2", Some(MatchKind::Url), None, None),
        ("https://github.com/WeakAuras", "url:https://github.com/WeakAuras", Some(MatchKind::Url), None, None),
        ("https://www.wowinterface.com/downloads/info5108-Clique.html", "wowi:5108", Some(MatchKind::Url), None, None),
        ("https://www.wowinterface.com/downloads/download5108-Clique", "wowi:5108", Some(MatchKind::Url), None, None),
        ("https://www.wowinterface.com/forums/", "ref_invalid", None, None, None),
        ("https://www.wowinterface.com/downloads/fileinfo.php?id=5995", "wowi:5995", Some(MatchKind::Url), None, None),
        ("https://wowinterface.com/downloads/fileinfo.php?s=abc&id=5995#comments", "wowi:5995", Some(MatchKind::Url), None, None),
        ("https://www.wowinterface.com/downloads/getfile.php?id=5995", "wowi:5995", Some(MatchKind::Url), None, None),
        ("https://cdn.wowinterface.com/downloads/getfile.php?id=5995&d=1791824100&minion", "wowi:5995", Some(MatchKind::Url), None, None),
        ("https://cdn.wowinterface.com/downloads/fileinfo.php?id=5995", "wowi:5995", Some(MatchKind::Url), None, None),
        ("https://www.wowinterface.com/downloads/fileinfo.php", "ref_invalid", None, None, None),
        ("https://www.wowinterface.com/downloads/fileinfo.php?id=59x5", "ref_invalid", None, None, None),
        ("https://cdn.wowinterface.com/downloads/getfile.php?d=1791824100", "ref_invalid", None, None, None),
        ("https://cdn.wowinterface.com/", "ref_invalid", None, None, None),
        ("https://www.tukui.org/addon/elvui", "tukui:elvui", Some(MatchKind::Url), None, None),
        ("https://tukui.org/download.php?ui=ElvUI", "tukui:elvui", Some(MatchKind::Url), None, None),
        ("https://www.curseforge.com/wow/addons/details", "source_disabled", None, None, None),
        ("https://addons.wago.io/addons/plater", "source_disabled", None, None, None),
        ("http://github.com/a/b", "ref_invalid", None, None, None),
        ("https://codeberg.org/a/b", "git:https://codeberg.org/a/b", Some(MatchKind::Url), None, None),
        ("https://example.org/repo.git#v2", "git:https://example.org/repo.git#v2", Some(MatchKind::Url), None, None),
        ("https://example.org/files/Addon-1.0.zip", "url:https://example.org/files/Addon-1.0.zip", Some(MatchKind::Url), None, None),
        ("https://example.org/dl?id=7", "url:https://example.org/dl?id=7", Some(MatchKind::Url), None, None),
        ("WeakAuras/WeakAuras2", "gh:WeakAuras/WeakAuras2", Some(MatchKind::Shorthand), None, None),
        ("a/b/c", "not_found", None, None, None),
        ("./missing.zip", "not_found", None, None, None),
        ("/definitely/not/here", "not_found", None, None, None),
        // Installed and catalog name the same repository (GitHub identity ignores case).
        ("weakauras", "gh:WeakAuras/WeakAuras2", Some(MatchKind::Installed), None, None),
        ("WEAKAURAS", "gh:WeakAuras/WeakAuras2", Some(MatchKind::Installed), None, None),
        ("details", "ref_ambiguous", None, None, None),
        ("elvui", "tukui:elvui", Some(MatchKind::Catalog), None, None),
        ("nonexistent-addon", "not_found", None, None, None),
        ("", "ref_invalid", None, None, None),
    ];

    #[test]
    fn every_input_class() {
        let dir = tempfile::tempdir().unwrap();
        with_ctx(dir.path(), |ctx| {
            for &(input, expected, kind, version, asset) in CASES {
                match parse_user_ref(input, ctx) {
                    Ok(p) => {
                        assert_eq!(p.r#ref.to_string(), expected, "{input:?}");
                        assert_eq!(
                            (
                                Some(p.matched_by),
                                p.version_hint.as_deref(),
                                p.asset_hint.as_deref()
                            ),
                            (kind, version, asset),
                            "{input:?}"
                        );
                    }
                    Err(e) => assert_eq!(e.code.wire(), expected, "{input:?}: {}", e.message),
                }
            }
        });
    }

    #[test]
    fn local_paths_and_collisions() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Addon.zip"), "PK").unwrap();
        std::fs::write(dir.path().join("notes.txt"), "x").unwrap();
        std::fs::create_dir_all(dir.path().join("owner/repo")).unwrap();
        std::fs::create_dir_all(dir.path().join("elvui")).unwrap();
        with_ctx(dir.path(), |ctx| {
            let zip = parse_user_ref("./Addon.zip", ctx).unwrap();
            assert_eq!(
                (zip.r#ref, zip.matched_by),
                (
                    Ref::File {
                        path: dir.path().join("./Addon.zip")
                    },
                    MatchKind::Path
                )
            );
            let abs = parse_user_ref(&dir.path().join("owner").display().to_string(), ctx).unwrap();
            assert_eq!(abs.matched_by, MatchKind::Path);
            assert_eq!(
                parse_user_ref("./notes.txt", ctx).unwrap_err().code,
                ErrorCode::RefInvalid
            );
            let canonical = parse_user_ref("file:Addon.zip", ctx).unwrap();
            assert_eq!(
                canonical.r#ref,
                Ref::File {
                    path: dir.path().join("Addon.zip")
                }
            );

            let err = parse_user_ref("owner/repo", ctx).unwrap_err();
            assert_eq!((err.code, err.exit().code()), (ErrorCode::RefAmbiguous, 3));
            assert_eq!(err.details["candidates"].as_array().unwrap().len(), 2);
            assert_eq!(err.fix.argv().unwrap()[1], "install");
            assert_eq!(err.alternatives[0].argv().unwrap()[2], "gh:owner/repo");

            let err = parse_user_ref("elvui", ctx).unwrap_err();
            assert_eq!(
                err.code,
                ErrorCode::RefAmbiguous,
                "a local folder and the catalog disagree"
            );
        });
    }

    #[test]
    fn ambiguity_lists_every_candidate_as_a_command() {
        let dir = tempfile::tempdir().unwrap();
        with_ctx(dir.path(), |ctx| {
            let err = parse_user_ref("details", ctx).unwrap_err();
            let candidates = err.details["candidates"].as_array().unwrap();
            assert_eq!(candidates.len(), 2);
            assert_eq!(candidates[0]["ref"], "gh:Tercioo/Details-Damage-Meter");
            assert!(
                candidates[0]["from"]
                    .as_str()
                    .unwrap()
                    .contains("installed")
            );
            assert_eq!(
                err.fix.argv().unwrap(),
                ["omaddons", "install", "gh:Tercioo/Details-Damage-Meter"]
            );
            assert_eq!(err.alternatives.len(), 1);
            assert_eq!(
                err.alternatives[0].argv().unwrap(),
                ["omaddons", "install", "wowi:23534"]
            );
        });
    }

    proptest! {
        #[test]
        fn never_panics_and_canonical_refs_round_trip(input in "\\PC{0,80}") {
            let dir = std::env::temp_dir();
            with_ctx(&dir, |ctx| {
                if let Ok(p) = parse_user_ref(&input, ctx) {
                    let text = p.r#ref.to_string();
                    let back: Ref = text.parse().unwrap();
                    assert_eq!(back, p.r#ref, "{input:?}");
                }
            });
        }

        #[test]
        fn url_shaped_input_never_panics(path in "[a-zA-Z0-9/._#?=&-]{0,60}") {
            let dir = std::env::temp_dir();
            with_ctx(&dir, |ctx| {
                for host in ["github.com", "www.wowinterface.com", "cdn.wowinterface.com", "tukui.org", "codeberg.org", "example.org"] {
                    let _ = parse_user_ref(&format!("https://{host}/{path}"), ctx);
                }
            });
        }
    }
}
