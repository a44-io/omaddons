//! `git:` refs to hosts other than GitHub, through the `git` binary (vision §5.4, §5.7).
//! A host must be listed in config `git_hosts` (else `host_blocked`), which HTTP never
//! reaches and git reaches alone: no host of HTTP's allowlist is git's. The URL is https
//! only. Resolution is `git ls-remote` and writes nothing; [`GitRunner::materialize`]
//! clones into a directory the engine created through fsx, the one place a subprocess
//! writes.
//!
//! Every subprocess goes through [`GitRunner::command`]: argv only, never a shell; an
//! environment cleared down to `PATH`, `HOME`, `LC_ALL=C`, `GIT_TERMINAL_PROMPT=0`,
//! `GIT_CONFIG_NOSYSTEM=1`, `GIT_CONFIG_GLOBAL=/dev/null` and `GIT_ALLOW_PROTOCOL`, so
//! no user or system config (`insteadOf`, proxies, credential helpers) can send a fetch
//! elsewhere; and `-c` options that refuse redirects, credentials and every protocol but
//! https. Each run touching a repository is one `via: git` line in `net.log` (the
//! `git --version` probe touches none). A run past its timeout is killed with its whole
//! process group, and so is a checkout that grows past the download limit
//! (`limits.download_max_bytes`). What git prints is kept up to `API_RESPONSE_MAX_BYTES`
//! per stream.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use super::Wanted;
use crate::error::{Error, ErrorCode, PlanStaleSource, Result, Suggestion};
use crate::fsx::{Op, ScopeTable, WriteSession, Writer, ensure_dir_all, mutate};
use crate::limits::API_RESPONSE_MAX_BYTES;
use crate::model::{Asset, AssetLocation, Channel, Ref, Release, ReleaseSet};
use crate::net::NetConfig;
use crate::net::allowlist::{GitHosts, is_http_host};
use crate::net::log::{GitLogEntry, NetLog};
use crate::politeness::{API_READ_TIMEOUT, DOWNLOAD_TIMEOUT};
use crate::state::paths::Paths;

/// The oldest git that honours `GIT_CONFIG_GLOBAL`.
const MIN_GIT: (u32, u32) = (2, 32);

/// How often a checkout's size is measured while git runs.
const WATCH_INTERVAL: Duration = Duration::from_millis(500);

/// Spawns tried again, 10 ms apart, while the binary is "text file busy".
const TXTBSY_TRIES: u32 = 50;

/// What git may speak: https in production; `file://` exists only for in-crate tests
/// against local repositories.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolPolicy {
    Https,
    #[cfg(test)]
    FileForTests,
}

impl ProtocolPolicy {
    const fn scheme(self) -> &'static str {
        match self {
            Self::Https => "https",
            #[cfg(test)]
            Self::FileForTests => "file",
        }
    }
}

/// Runs git for one command invocation.
pub struct GitRunner {
    /// The hosts git may reach: exactly config `git_hosts`.
    hosts: GitHosts,
    offline: bool,
    log: Option<NetLog>,
    program: OsString,
    policy: ProtocolPolicy,
    /// The `git --version` verdict, probed once.
    version: OnceLock<std::result::Result<(), Error>>,
    /// The most a checkout may hold, `.git` included (`limits.download_max_bytes`).
    download_max_bytes: u64,
    /// The most kept of each of git's output streams; the rest is read and dropped.
    output_max: u64,
    /// git's `HOME`: an empty directory omaddons owns (`None` in in-crate tests that
    /// need none).
    home: Option<GitHome>,
}

/// `<cache>/omaddons/git-home/`, made ready (created 0700 through fsx, checked empty)
/// once, before the first run that touches a repository.
struct GitHome {
    paths: Paths,
    ready: OnceLock<std::result::Result<(), Error>>,
}

impl GitHome {
    fn new(paths: &Paths) -> Self {
        Self {
            paths: paths.clone(),
            ready: OnceLock::new(),
        }
    }

    fn dir(&self) -> PathBuf {
        self.paths.git_home()
    }

    /// Creates it (0700, through fsx) and checks it is a directory with nothing in it:
    /// a file there could be a `.netrc` or a git setting git would then obey.
    fn ensure(&self) -> Result<()> {
        self.ready
            .get_or_init(|| {
                let dir = self.dir();
                let scopes = ScopeTable::new(&self.paths, &[], None)?;
                let mut s = WriteSession::untracked(Writer::Derived, &scopes, "git-home");
                ensure_dir_all(&mut s, &dir)?;
                mutate(&mut s, &dir, Op::Chmod { mode: 0o700 })?;
                let io = |e: std::io::Error| Error::io(&dir, &e, Suggestion::text("check the cache directory"));
                let meta = std::fs::symlink_metadata(&dir).map_err(io)?;
                let empty = meta.is_dir() && std::fs::read_dir(&dir).map_err(io)?.next().is_none();
                if !empty {
                    return Err(Error::new(
                        ErrorCode::Io,
                        format!("{} is not an empty directory", dir.display()),
                        Suggestion::text(
                            "empty this directory; omaddons uses it as git's home so no personal git or curl files apply",
                        ),
                    )
                    .details(serde_json::json!({ "path": dir })));
                }
                Ok(())
            })
            .clone()
    }
}

impl std::fmt::Debug for GitRunner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitRunner")
            .field("offline", &self.offline)
            .field("program", &self.program)
            .field("policy", &self.policy)
            .finish_non_exhaustive()
    }
}

/// A repository URL split for the allowlist and the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoUrl {
    pub url: String,
    /// Lowercase, without a port.
    pub host: String,
    /// Without a query or fragment.
    pub path: String,
}

/// What one git run printed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitOutput {
    pub stdout: String,
    pub stderr: String,
}

impl GitRunner {
    /// For a run with `config`'s git hosts and offline flag, logging to `log`, with its
    /// home under `paths`' cache.
    pub fn new(config: &NetConfig, log: NetLog, paths: &Paths) -> Self {
        Self {
            hosts: config.git_allowlist(),
            offline: config.offline,
            log: Some(log),
            program: OsString::from("git"),
            policy: ProtocolPolicy::Https,
            version: OnceLock::new(),
            download_max_bytes: config.download_max_bytes,
            output_max: API_RESPONSE_MAX_BYTES,
            home: Some(GitHome::new(paths)),
        }
    }

    /// A runner for in-crate tests: `file://` repositories, no allowlist, no log.
    #[cfg(test)]
    pub(crate) fn for_tests() -> Self {
        Self {
            hosts: NetConfig::default().git_allowlist(),
            offline: false,
            log: None,
            program: OsString::from("git"),
            policy: ProtocolPolicy::FileForTests,
            version: OnceLock::new(),
            download_max_bytes: crate::limits::DOWNLOAD_MAX_BYTES,
            output_max: API_RESPONSE_MAX_BYTES,
            home: None,
        }
    }

    /// The same runner with its home under `paths`' cache.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_home(mut self, paths: &Paths) -> Self {
        self.home = Some(GitHome::new(paths));
        self
    }

    /// The same runner with checkouts held to `bytes`.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_download_max_bytes(mut self, bytes: u64) -> Self {
        self.download_max_bytes = bytes;
        self
    }

    /// The hygienic command for `args`, before it is spawned.
    pub fn command(&self, args: &[&OsStr]) -> Command {
        // Always "git" in production; only in-crate tests substitute a stand-in.
        let mut cmd = Command::new(&self.program); // ubs:ignore — fixed executable
        cmd.env_clear();
        if let Some(path) = std::env::var_os("PATH") {
            cmd.env("PATH", path);
        }
        // Never the user's home: libcurl would read its `.netrc` (git always asks it to).
        if let Some(home) = &self.home {
            cmd.env("HOME", home.dir())
                .env("XDG_CONFIG_HOME", home.dir());
        }
        let scheme = self.policy.scheme();
        cmd.env("LC_ALL", "C")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_ALLOW_PROTOCOL", scheme);
        for option in [
            "http.followRedirects=false".to_owned(),
            "credential.helper=".to_owned(),
            "core.askPass=".to_owned(),
            "protocol.allow=never".to_owned(),
            format!("protocol.{scheme}.allow=always"),
        ] {
            cmd.arg("-c").arg(option);
        }
        cmd.args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        cmd
    }

    /// `url` split, when its scheme is the policy's and it carries no credentials.
    pub fn repo_url(&self, url: &str) -> Result<RepoUrl> {
        let invalid = |why: &str| {
            Error::new(
                ErrorCode::RefInvalid,
                format!("git:{url}: {why}"),
                Suggestion::command(["omaddons", "help", "install"]),
            )
        };
        let prefix = format!("{}://", self.policy.scheme());
        let rest = url
            .get(..prefix.len())
            .filter(|p| p.eq_ignore_ascii_case(&prefix))
            .and_then(|_| url.get(prefix.len()..))
            .ok_or_else(|| invalid("git refs need an https:// URL"))?;
        let (authority, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
        if authority.contains('@') {
            return Err(invalid("credentials in a git URL are not supported"));
        }
        let host = authority
            .split(':')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        Ok(RepoUrl {
            url: url.to_owned(),
            host,
            path: path.split(['?', '#']).next().unwrap_or(path).to_owned(),
        })
    }

    /// One git run on `repo`: refused offline, for unlisted hosts or without a usable
    /// git; killed after `timeout`; logged.
    pub fn run(
        &self,
        args: &[&OsStr],
        repo: &RepoUrl,
        purpose: &'static str,
        timeout: Duration,
    ) -> Result<GitOutput> {
        self.run_watched(args, repo, purpose, timeout, None)
    }

    /// [`GitRunner::run`], also killed once `checkout`, the directory it writes, grows
    /// past the download limit (`archive_too_large`).
    fn run_watched(
        &self,
        args: &[&OsStr],
        repo: &RepoUrl,
        purpose: &'static str,
        timeout: Duration,
        checkout: Option<&Path>,
    ) -> Result<GitOutput> {
        if self.offline {
            return Err(Error::new(
                ErrorCode::Offline,
                format!("offline: not running git {purpose} for {}", repo.host),
                Suggestion::text("run without --offline and with OMADDONS_OFFLINE unset"),
            ));
        }
        if self.policy == ProtocolPolicy::Https && !self.hosts.permits(&repo.host) {
            let http = if is_http_host(&repo.host) {
                " (it is allowed for HTTP downloads, not for the git binary)"
            } else {
                ""
            };
            return Err(Error::new(
                ErrorCode::HostBlocked,
                format!("{} is not a git host omaddons may contact{http}", repo.host),
                Suggestion::text(format!(
                    "add it to git_hosts: omaddons config set git_hosts '[\"{}\"]' (keep any hosts already listed)",
                    repo.host
                )),
            )
            .details(serde_json::json!({ "host": repo.host })));
        }
        self.check_version()?;
        if let Some(home) = &self.home {
            home.ensure()?;
        }
        let started = Instant::now();
        let watch = checkout.map(|dir| Watch {
            dir,
            cap: self.download_max_bytes,
            repo,
        });
        let result = finish(self.command(args), timeout, purpose, self.output_max, watch);
        let (exit_code, error) = match &result {
            Ok((status, _)) => (status.code(), None),
            Err(e) => (None, Some(e.code)),
        };
        // Every run is logged, or fails as an unlogged HTTP request does; its output is
        // then dropped, success or not.
        if let Some(log) = &self.log {
            log.record_git(GitLogEntry {
                host: repo.host.clone(),
                path: repo.path.clone(),
                exit_code,
                duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                purpose,
                error,
            })?;
        }
        let (status, output) = result?;
        if status.success() {
            Ok(output)
        } else {
            Err(failure(purpose, repo, &output.stderr))
        }
    }

    /// `git --version`, once: a missing git or one older than 2.32 is `source_disabled`.
    fn check_version(&self) -> Result<()> {
        self.version
            .get_or_init(|| {
                let (status, output) = finish(
                    self.command(&[OsStr::new("--version")]),
                    API_READ_TIMEOUT,
                    "version",
                    self.output_max,
                    None,
                )?;
                let found = status
                    .success()
                    .then(|| parse_version(&output.stdout))
                    .flatten();
                match found {
                    Some(v) if v >= MIN_GIT => Ok(()),
                    _ => Err(Error::new(
                        ErrorCode::SourceDisabled,
                        format!(
                            "git refs need git 2.32 or newer; this git reports {:?}",
                            output.stdout.trim()
                        ),
                        Suggestion::text("upgrade git (pacman -S git)"),
                    )),
                }
            })
            .clone()
    }

    /// Clones `rev` at `sha` from `url` into `dest`, an empty directory the caller
    /// created: shallow for a branch or tag, a fetch by sha otherwise. `plan_stale` when
    /// the checkout is not `sha` (the branch moved since resolution); `archive_too_large`
    /// when it grows past the download limit, `.git` included.
    pub fn materialize(&self, url: &str, rev: &str, sha: &str, dest: &Path) -> Result<()> {
        let repo = self.repo_url(url)?;
        let dest_arg = dest.as_os_str();
        let os = OsStr::new;
        let watched = Some(dest);
        if rev.eq_ignore_ascii_case(sha) {
            self.run(
                &[os("init"), os("--quiet"), dest_arg],
                &repo,
                "init",
                API_READ_TIMEOUT,
            )?;
            self.run_watched(
                &[os("-C"), dest_arg, os("fetch"), os("--quiet"), os("--depth"), os("1"), os("--"), os(url), os(sha)],
                &repo,
                "fetch",
                DOWNLOAD_TIMEOUT,
                watched,
            )
            .map_err(|e| {
                if e.code == ErrorCode::DownloadFailed {
                    Error::new(
                        ErrorCode::DownloadFailed,
                        format!("{url} refused to send commit {sha}; some servers only serve branches and tags"),
                        Suggestion::text("name a branch or tag after # instead of a commit"),
                    )
                } else {
                    e
                }
            })?;
            self.run_watched(
                &[
                    os("-C"),
                    dest_arg,
                    os("checkout"),
                    os("--quiet"),
                    os("--detach"),
                    os("FETCH_HEAD"),
                ],
                &repo,
                "checkout",
                API_READ_TIMEOUT,
                watched,
            )?;
        } else {
            let branch = format!("--branch={rev}");
            self.run_watched(
                &[
                    os("clone"),
                    os("--quiet"),
                    os("--depth"),
                    os("1"),
                    os(branch.as_str()),
                    os("--single-branch"),
                    os("--no-tags"),
                    os("--"),
                    os(url),
                    dest_arg,
                ],
                &repo,
                "clone",
                DOWNLOAD_TIMEOUT,
                watched,
            )?;
        }
        // A fast run can finish between two measures.
        if tree_bytes(dest, self.download_max_bytes) > self.download_max_bytes {
            return Err(checkout_too_large(&repo, self.download_max_bytes));
        }
        let head = self.run(
            &[os("-C"), dest_arg, os("rev-parse"), os("HEAD")],
            &repo,
            "rev-parse",
            API_READ_TIMEOUT,
        )?;
        let head = head.stdout.trim();
        if !head.eq_ignore_ascii_case(sha) {
            return Err(Error::plan_stale(
                PlanStaleSource::Replan,
                format!("{url}#{rev} moved from {sha} to {head} since it was resolved"),
                Suggestion::text("run the command again"),
            ));
        }
        Ok(())
    }

    /// The release set of a `git:` ref on this host (resolution only; writes nothing).
    pub fn fetch(&self, w: &Wanted) -> Result<ReleaseSet> {
        let Ref::Git { url, rev } = &w.r#ref else {
            return Err(Error::internal(format!("{} is not a git ref", w.r#ref)));
        };
        let repo = self.repo_url(url)?;
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
            Some(rev) if is_sha(rev) => Resolved {
                rev: rev.to_ascii_lowercase(),
                sha: rev.to_ascii_lowercase(),
                tag: false,
            },
            Some(rev) => {
                let patterns = [
                    format!("refs/heads/{rev}"),
                    format!("refs/tags/{rev}"),
                    format!("refs/tags/{rev}^{{}}"),
                ];
                let mut args: Vec<&OsStr> =
                    vec![OsStr::new("ls-remote"), OsStr::new("--"), OsStr::new(url)];
                args.extend(patterns.iter().map(OsStr::new));
                let out = self.run(&args, &repo, "ls-remote", API_READ_TIMEOUT)?;
                resolve(&parse_ls_remote(&out.stdout), Some(rev))
                    .ok_or_else(|| not_found(w, Some(rev)))?
            }
            None => {
                let args = [
                    OsStr::new("ls-remote"),
                    OsStr::new("--symref"),
                    OsStr::new("--"),
                    OsStr::new(url),
                    OsStr::new("HEAD"),
                ];
                let out = self.run(&args, &repo, "ls-remote", API_READ_TIMEOUT)?;
                resolve(&parse_ls_remote(&out.stdout), None).ok_or_else(|| not_found(w, None))?
            }
        };
        Ok(release_set(w, url, &repo, &resolved))
    }
}

/// What a rev resolved to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    /// The branch or tag to check out, or the sha itself.
    pub rev: String,
    pub sha: String,
    pub tag: bool,
}

/// What `git ls-remote [--symref]` printed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RemoteRefs {
    /// The branch `HEAD` points at.
    pub head_branch: Option<String>,
    pub head: Option<String>,
    /// Ref name to sha, peeled tags as `refs/tags/<t>^{}`.
    pub refs: BTreeMap<String, String>,
}

/// Parses `ls-remote` output: `ref: refs/heads/<b>\tHEAD` and `<sha>\t<name>` lines;
/// anything else is ignored.
pub fn parse_ls_remote(text: &str) -> RemoteRefs {
    let mut out = RemoteRefs::default();
    for line in text.lines() {
        let Some((left, name)) = line.split_once('\t') else {
            continue;
        };
        let name = name.trim();
        if let Some(target) = left.strip_prefix("ref: ") {
            if name == "HEAD" {
                out.head_branch = target.strip_prefix("refs/heads/").map(str::to_owned);
            }
            continue;
        }
        if !is_sha(left) {
            continue;
        }
        let sha = left.to_ascii_lowercase();
        if name == "HEAD" {
            out.head = Some(sha);
        } else {
            out.refs.insert(name.to_owned(), sha);
        }
    }
    out
}

/// A rev's commit: a branch before a tag (a peeled tag's commit before the tag object);
/// no rev is `HEAD` on its branch.
pub fn resolve(refs: &RemoteRefs, rev: Option<&str>) -> Option<Resolved> {
    let Some(rev) = rev else {
        let sha = refs.head.clone()?;
        let rev = refs.head_branch.clone().unwrap_or_else(|| sha.clone());
        return Some(Resolved {
            rev,
            sha,
            tag: false,
        });
    };
    if let Some(sha) = refs.refs.get(&format!("refs/heads/{rev}")) {
        return Some(Resolved {
            rev: rev.to_owned(),
            sha: sha.clone(),
            tag: false,
        });
    }
    let peeled = refs.refs.get(&format!("refs/tags/{rev}^{{}}"));
    let tag = peeled.or_else(|| refs.refs.get(&format!("refs/tags/{rev}")))?;
    Some(Resolved {
        rev: rev.to_owned(),
        sha: tag.clone(),
        tag: true,
    })
}

/// A full commit sha.
pub fn is_sha(rev: &str) -> bool {
    rev.len() == 40 && rev.chars().all(|c| c.is_ascii_hexdigit())
}

/// `(major, minor)` of `git version 2.55.0` (vendor suffixes allowed).
pub fn parse_version(text: &str) -> Option<(u32, u32)> {
    let version = text.trim().strip_prefix("git version ")?;
    let mut parts = version.split(['.', ' ']);
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    Some((major, minor))
}

fn not_found(w: &Wanted, rev: Option<&str>) -> Error {
    let what = rev.map_or_else(
        || "a default branch".to_owned(),
        |r| format!("a branch or tag named {r}"),
    );
    Error::new(
        ErrorCode::NotFound,
        format!("{} has no {what}", w.r#ref),
        Suggestion::text("check the branch or tag name, or give a commit sha after #"),
    )
}

/// One release with one checkout asset; the archive name keeps the full sha so archive
/// inspection names a repository-root addon by its TOC.
fn release_set(w: &Wanted, url: &str, repo: &RepoUrl, r: &Resolved) -> ReleaseSet {
    let short: String = r.sha.chars().take(7).collect();
    let name = repo
        .path
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("repo")
        .trim_end_matches(".git")
        .to_owned();
    ReleaseSet {
        r#ref: w.r#ref.clone(),
        releases: vec![Release {
            release_id: format!("git:{url}@{}", r.sha),
            version: if r.tag { r.rev.clone() } else { short },
            channel: Channel::Stable,
            published_at: None,
            assets: vec![Asset {
                filename: format!("{name}-{}", r.sha),
                location: AssetLocation::GitCheckout {
                    url: url.to_owned(),
                    rev: r.rev.clone(),
                    sha: r.sha.clone(),
                },
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

/// A failed git run as a typed error.
fn failure(purpose: &str, repo: &RepoUrl, stderr: &str) -> Error {
    let last = stderr
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    let lower = stderr.to_ascii_lowercase();
    let message = format!(
        "git {purpose} failed for {}{}: {last}",
        repo.host, repo.path
    );
    if lower.contains("terminal prompts disabled")
        || lower.contains("authentication")
        || lower.contains("could not read username")
    {
        return Error::new(
            ErrorCode::DownloadFailed,
            format!(
                "{message} (the repository needs a login; private repositories are not supported)"
            ),
            Suggestion::text(
                "use a public repository, or download the addon and: omaddons install <path>",
            ),
        )
        .retryable(false);
    }
    if lower.contains("not found") || lower.contains("does not appear to be a git repository") {
        return Error::new(
            ErrorCode::NotFound,
            message,
            Suggestion::text("check the repository URL"),
        );
    }
    if purpose == "clone" || purpose == "fetch" {
        return Error::new(
            ErrorCode::DownloadFailed,
            message,
            Suggestion::text("check the URL and rev, then retry"),
        )
        .retryable(false);
    }
    Error::new(ErrorCode::Network, message, Suggestion::text("retry later")).retryable(true)
}

/// A directory a git run writes, held to `cap` bytes.
struct Watch<'a> {
    dir: &'a Path,
    cap: u64,
    repo: &'a RepoUrl,
}

/// `archive_too_large` for a checkout of `repo` past the download limit `cap`.
fn checkout_too_large(repo: &RepoUrl, cap: u64) -> Error {
    Error::new(
        ErrorCode::ArchiveTooLarge,
        format!(
            "the git checkout of {}{} grew past the {cap}-byte download limit",
            repo.host, repo.path
        ),
        Suggestion::text(
            "if the repository is genuine, raise the limit: omaddons config set limits.download_max_bytes <value>",
        ),
    )
    .details(serde_json::json!({ "limit": cap, "config_key": "limits.download_max_bytes" }))
}

/// The bytes under `dir` (every entry's own length, symlinks not followed), counted
/// until past `past`. An entry that cannot be read counts nothing.
fn tree_bytes(dir: &Path, past: u64) -> u64 {
    let mut total = 0u64;
    let mut pending = vec![dir.to_path_buf()];
    while let Some(next) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&next) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(meta) = std::fs::symlink_metadata(entry.path()) else {
                continue;
            };
            total = total.saturating_add(meta.len());
            if total > past {
                return total;
            }
            if meta.is_dir() {
                pending.push(entry.path());
            }
        }
    }
    total
}

/// Reads `pipe` to its end, keeping its first `max` bytes: the rest is dropped, so the
/// writer never blocks on a full pipe.
fn read_bounded(pipe: Option<Box<dyn Read + Send>>, max: u64) -> String {
    let mut kept = Vec::new();
    if let Some(mut pipe) = pipe {
        let _ = pipe.by_ref().take(max).read_to_end(&mut kept);
        let _ = std::io::copy(&mut pipe, &mut std::io::sink());
    }
    String::from_utf8_lossy(&kept).into_owned()
}

/// Spawns `cmd`, collects up to `output_max` bytes of each of its outputs and waits at
/// most `timeout`, killing its process group past that, or once `watch`'s directory
/// grows past its cap (measured every `WATCH_INTERVAL`). A missing git is
/// `source_disabled`; a timeout is `network`; a checkout past its cap
/// `archive_too_large`.
fn finish(
    mut cmd: Command,
    timeout: Duration,
    purpose: &str,
    output_max: u64,
    watch: Option<Watch<'_>>,
) -> Result<(ExitStatus, GitOutput)> {
    // A binary being replaced at this moment (a package upgrade) is "text file busy"
    // for a moment: tried again briefly.
    let mut tries = 0;
    let spawned = loop {
        match cmd.spawn() {
            Err(e)
                if tries < TXTBSY_TRIES
                    && rustix::io::Errno::from_io_error(&e) == Some(rustix::io::Errno::TXTBSY) =>
            {
                tries += 1;
                std::thread::sleep(Duration::from_millis(10));
            }
            spawned => break spawned,
        }
    };
    let mut child = spawned.map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            Error::new(
                ErrorCode::SourceDisabled,
                "git refs to hosts other than GitHub need git, which is not installed",
                Suggestion::text("install git (pacman -S git)"),
            )
        } else {
            Error::new(
                ErrorCode::SourceDisabled,
                format!("git could not be started: {e}"),
                Suggestion::text("check that git runs: git --version"),
            )
        }
    })?;
    let reader = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || read_bounded(pipe, output_max))
    };
    let out = reader(
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let err = reader(
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    // Kills git with its whole process group.
    let stop = |child: &mut std::process::Child| {
        if let Some(pid) = i32::try_from(child.id())
            .ok()
            .and_then(rustix::process::Pid::from_raw)
        {
            let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
        }
        let _ = child.kill();
        let _ = child.wait();
    };
    let deadline = Instant::now() + timeout;
    let mut measure_at = Instant::now() + WATCH_INTERVAL;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                stop(&mut child);
                return Err(Error::new(
                    ErrorCode::Network,
                    format!(
                        "git {purpose} took longer than {} s and was stopped",
                        timeout.as_secs()
                    ),
                    Suggestion::text("retry later; a slow or unreachable host stops git"),
                )
                .retryable(true));
            }
            Ok(None) if Instant::now() >= measure_at => {
                if let Some(w) = &watch
                    && tree_bytes(w.dir, w.cap) > w.cap
                {
                    stop(&mut child);
                    return Err(checkout_too_large(w.repo, w.cap));
                }
                measure_at = Instant::now() + WATCH_INTERVAL;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(e) => {
                return Err(Error::new(
                    ErrorCode::Network,
                    format!("waiting for git {purpose} failed: {e}"),
                    Suggestion::text("retry"),
                ));
            }
        }
    };
    let stdout = out.join().unwrap_or_default();
    let stderr = err.join().unwrap_or_default();
    Ok((status, GitOutput { stdout, stderr }))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    const A: &str = "1111111111111111111111111111111111111111";
    const B: &str = "2222222222222222222222222222222222222222";
    const T: &str = "3333333333333333333333333333333333333333";

    fn runner(policy: ProtocolPolicy, hosts: &[&str]) -> GitRunner {
        let config = NetConfig {
            git_hosts: hosts.iter().map(|h| (*h).to_owned()).collect(),
            ..NetConfig::default()
        };
        GitRunner {
            hosts: config.git_allowlist(),
            offline: false,
            log: None,
            program: OsString::from("git"),
            policy,
            version: OnceLock::new(),
            download_max_bytes: crate::limits::DOWNLOAD_MAX_BYTES,
            output_max: API_RESPONSE_MAX_BYTES,
            home: None,
        }
    }

    /// Paths rooted in `dir` (a temp dir), for a runner's home and log.
    fn paths_in(dir: &Path) -> Paths {
        let root = std::fs::canonicalize(dir).unwrap();
        Paths::with_roots(
            &root,
            root.join(".config"),
            root.join(".local/state"),
            root.join(".cache"),
        )
    }

    /// A stand-in for git at `<dir>/git` running `script`.
    fn stand_in(dir: &Path, script: &str) -> OsString {
        let fake = dir.join("git");
        fs::write(&fake, format!("#!/bin/sh\n{script}")).unwrap();
        fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
        fake.into_os_string()
    }

    #[test]
    fn ls_remote_output() {
        let text = format!(
            "ref: refs/heads/main\tHEAD\n{A}\tHEAD\n{A}\trefs/heads/main\n{B}\trefs/heads/dev\n{T}\trefs/tags/v1\n{B}\trefs/tags/v1^{{}}\n{A}\trefs/tags/light\nwarning: junk\n"
        );
        let refs = parse_ls_remote(&text);
        assert_eq!(
            (refs.head_branch.as_deref(), refs.head.as_deref()),
            (Some("main"), Some(A))
        );
        let r = |rev| resolve(&refs, rev).map(|r| (r.rev, r.sha, r.tag));
        assert_eq!(r(None), Some(("main".into(), A.into(), false)));
        assert_eq!(r(Some("dev")), Some(("dev".into(), B.into(), false)));
        assert_eq!(r(Some("v1")), Some(("v1".into(), B.into(), true)), "peeled");
        assert_eq!(
            r(Some("light")),
            Some(("light".into(), A.into(), true)),
            "unpeeled"
        );
        assert_eq!(r(Some("nope")), None);
        // A branch and a tag sharing a name resolve to the branch.
        let both = parse_ls_remote(&format!("{A}\trefs/heads/x\n{B}\trefs/tags/x\n"));
        assert_eq!(resolve(&both, Some("x")).map(|r| r.tag), Some(false));
        assert_eq!(parse_ls_remote(""), RemoteRefs::default());
        assert_eq!(resolve(&RemoteRefs::default(), None), None);
    }

    #[test]
    fn versions_and_urls() {
        assert_eq!(parse_version("git version 2.55.0\n"), Some((2, 55)));
        assert_eq!(parse_version("git version 2.31.1.windows.1"), Some((2, 31)));
        assert_eq!(parse_version("hg 6"), None);
        let git = runner(ProtocolPolicy::Https, &["codeberg.org"]);
        let repo = git
            .repo_url("https://Codeberg.org:443/o/r.git?x=1")
            .unwrap();
        assert_eq!(
            (repo.host.as_str(), repo.path.as_str()),
            ("codeberg.org", "/o/r.git")
        );
        assert_eq!(
            git.repo_url("https://user:pw@codeberg.org/o/r")
                .unwrap_err()
                .code,
            ErrorCode::RefInvalid
        );
        assert_eq!(
            git.repo_url("file:///tmp/r").unwrap_err().code,
            ErrorCode::RefInvalid
        );
    }

    #[test]
    fn the_command_is_hygienic() {
        // Junk in the parent environment must not reach git, nor the user's home.
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());
        let mut git = runner(ProtocolPolicy::Https, &[]);
        git.home = Some(GitHome::new(&paths));
        let cmd = git.command(&[OsStr::new("ls-remote")]);
        let envs: BTreeMap<String, Option<String>> = cmd
            .get_envs()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.map(|v| v.to_string_lossy().into_owned()),
                )
            })
            .collect();
        let keys: Vec<&str> = envs.keys().map(String::as_str).collect();
        let mut expected = vec![
            "GIT_ALLOW_PROTOCOL",
            "GIT_CONFIG_GLOBAL",
            "GIT_CONFIG_NOSYSTEM",
            "GIT_TERMINAL_PROMPT",
            "HOME",
            "LC_ALL",
            "XDG_CONFIG_HOME",
        ];
        expected.extend(["PATH"].iter().filter(|v| std::env::var_os(v).is_some()));
        expected.sort_unstable();
        assert_eq!(keys, expected);
        let home = paths.git_home().display().to_string();
        assert_eq!(envs["HOME"].as_deref(), Some(home.as_str()));
        assert_eq!(envs["XDG_CONFIG_HOME"].as_deref(), Some(home.as_str()));
        assert_ne!(envs["HOME"].clone(), std::env::var("HOME").ok());
        assert_eq!(envs["GIT_CONFIG_GLOBAL"].as_deref(), Some("/dev/null"));
        assert_eq!(envs["GIT_ALLOW_PROTOCOL"].as_deref(), Some("https"));
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                "-c",
                "http.followRedirects=false",
                "-c",
                "credential.helper=",
                "-c",
                "core.askPass=",
                "-c",
                "protocol.allow=never",
                "-c",
                "protocol.https.allow=always",
                "ls-remote",
            ]
        );
        assert_eq!(cmd.get_program(), "git");
    }

    #[test]
    fn the_spawned_environment_is_exactly_the_cleared_one() {
        // A script stands in for git and prints the environment it received.
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("git");
        fs::write(&fake, "#!/bin/sh\nexec env\n").unwrap();
        fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
        let mut git = runner(ProtocolPolicy::Https, &[]);
        git.program = fake.into_os_string();
        let paths = paths_in(dir.path());
        git.home = Some(GitHome::new(&paths));
        let (status, out) = finish(
            git.command(&[]),
            Duration::from_secs(10),
            "env",
            API_RESPONSE_MAX_BYTES,
            None,
        )
        .unwrap();
        assert!(status.success());
        let mut keys: Vec<&str> = out
            .stdout
            .lines()
            .filter_map(|l| l.split_once('=').map(|(k, _)| k))
            .collect();
        keys.sort_unstable();
        keys.dedup();
        assert!(
            keys.iter().all(|k| [
                "GIT_ALLOW_PROTOCOL",
                "GIT_CONFIG_GLOBAL",
                "GIT_CONFIG_NOSYSTEM",
                "GIT_TERMINAL_PROMPT",
                "HOME",
                "LC_ALL",
                "PATH",
                "XDG_CONFIG_HOME",
                // Set by the stand-in shell itself, not inherited.
                "PWD",
                "SHLVL",
                "_",
            ]
            .contains(k)),
            "{keys:?}"
        );
        assert!(keys.contains(&"GIT_CONFIG_GLOBAL"), "{keys:?}");
        let home = format!("HOME={}", paths.git_home().display());
        assert!(out.stdout.lines().any(|l| l == home), "{}", out.stdout);
    }

    /// git's home is created empty (0700) through fsx before the first repository run;
    /// one with anything in it refuses every run, since a `.netrc` there would be read.
    #[test]
    fn the_git_home_is_created_empty_and_a_used_one_refuses() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());
        let mut git = runner(ProtocolPolicy::Https, &["codeberg.org"]);
        git.program = stand_in(dir.path(), VERSIONED);
        git.home = Some(GitHome::new(&paths));
        let repo = git.repo_url("https://codeberg.org/o/r").unwrap();
        git.run(&[], &repo, "ls-remote", API_READ_TIMEOUT).unwrap();
        let home = paths.git_home();
        let meta = fs::metadata(&home).unwrap();
        assert!(meta.is_dir());
        assert_eq!(meta.permissions().mode() & 0o777, 0o700);
        assert_eq!(fs::read_dir(&home).unwrap().count(), 0);

        fs::write(
            home.join(".netrc"),
            "machine codeberg.org login me password x\n",
        )
        .unwrap();
        let mut used = runner(ProtocolPolicy::Https, &["codeberg.org"]);
        used.program = git.program.clone();
        used.home = Some(GitHome::new(&paths));
        let err = used
            .run(&[], &repo, "ls-remote", API_READ_TIMEOUT)
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::Io);
        assert_eq!(err.details["path"], serde_json::json!(home));
    }

    /// A git run whose `net.log` line cannot be written fails, as an HTTP request does.
    #[test]
    fn a_run_that_cannot_be_logged_fails() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());
        let mut git = runner(ProtocolPolicy::Https, &["codeberg.org"]);
        git.program = stand_in(dir.path(), VERSIONED);
        git.log = Some(NetLog::new(&paths).unwrap());
        let repo = git.repo_url("https://codeberg.org/o/r").unwrap();
        // Logged, the run succeeds.
        git.run(&[], &repo, "ls-remote", API_READ_TIMEOUT).unwrap();
        // net.log made a directory: no line can be appended to it, and the run fails
        // with that error although git exited 0.
        fs::remove_file(paths.net_log()).unwrap();
        fs::create_dir_all(paths.net_log()).unwrap();
        let err = git
            .run(&[], &repo, "ls-remote", API_READ_TIMEOUT)
            .unwrap_err();
        assert!(
            err.message.contains("net.log"),
            "{:?}: {}",
            err.code,
            err.message
        );
    }

    /// A stand-in git that knows its version and succeeds at everything else.
    const VERSIONED: &str =
        "for a in \"$@\"; do [ \"$a\" = --version ] && echo 'git version 2.51.0'; done\nexit 0\n";

    #[test]
    fn refusals_spawn_nothing() {
        let mut git = runner(ProtocolPolicy::Https, &["codeberg.org"]);
        git.program = OsString::from("/nonexistent/git");
        let repo = git.repo_url("https://gitlab.com/o/r").unwrap();
        let err = git
            .run(&[], &repo, "ls-remote", API_READ_TIMEOUT)
            .unwrap_err();
        assert_eq!((err.code, err.exit().code()), (ErrorCode::HostBlocked, 4));
        assert!(err.fix.argv().is_none());
        assert!(err.fix.display().contains("git_hosts"));
        // HTTP's hosts are never git's, even with no git_hosts at all.
        let mut none = runner(ProtocolPolicy::Https, &[]);
        none.program = OsString::from("/nonexistent/git");
        for url in [
            "https://api.tukui.org/v1/x",
            "https://cdn.wowinterface.com/x",
            "https://codeload.github.com/x",
            "https://github.com/o/r",
        ] {
            let repo = none.repo_url(url).unwrap();
            let err = none
                .run(&[], &repo, "ls-remote", API_READ_TIMEOUT)
                .unwrap_err();
            assert_eq!(err.code, ErrorCode::HostBlocked, "{url}");
            assert!(
                err.message
                    .contains("allowed for HTTP downloads, not for the git binary"),
                "{url}: {}",
                err.message
            );
        }
        git.offline = true;
        let repo = git.repo_url("https://codeberg.org/o/r").unwrap();
        let err = git
            .run(&[], &repo, "ls-remote", API_READ_TIMEOUT)
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::Offline);
        // Allowed and online, a missing git is source_disabled with the package fix.
        git.offline = false;
        let err = git
            .run(&[], &repo, "ls-remote", API_READ_TIMEOUT)
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::SourceDisabled);
        assert_eq!(err.fix.display(), "install git (pacman -S git)");
    }

    #[test]
    fn a_slow_git_is_killed_with_its_children() {
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("git");
        fs::write(&fake, "#!/bin/sh\nsleep 30 &\nwait\n").unwrap();
        fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
        let mut git = runner(ProtocolPolicy::Https, &[]);
        git.program = fake.into_os_string();
        let started = Instant::now();
        let err = finish(
            git.command(&[]),
            Duration::from_millis(300),
            "ls-remote",
            API_RESPONSE_MAX_BYTES,
            None,
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::Network);
        assert!(err.retryable);
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the sleeping child was killed too"
        );
    }

    /// A git that keeps writing into the checkout is stopped, with the child doing the
    /// writing, once the checkout passes the cap: long before its timeout.
    #[test]
    fn a_growing_checkout_is_killed_with_its_children() {
        let dir = tempfile::tempdir().unwrap();
        let checkout = dir.path().join("checkout");
        fs::create_dir_all(&checkout).unwrap();
        let grown = checkout.join("pack");
        let mut git = runner(ProtocolPolicy::Https, &["codeberg.org"]);
        git.program = stand_in(
            dir.path(),
            &format!(
                "(while :; do head -c 65536 /dev/zero >> '{}'; sleep 0.02; done) &\nwait\n",
                grown.display()
            ),
        );
        let repo = git.repo_url("https://codeberg.org/o/r").unwrap();
        let watch = Watch {
            dir: &checkout,
            cap: 1 << 20,
            repo: &repo,
        };
        let started = Instant::now();
        let err = finish(
            git.command(&[]),
            Duration::from_secs(60),
            "clone",
            API_RESPONSE_MAX_BYTES,
            Some(watch),
        )
        .unwrap_err();
        assert_eq!(
            (err.code, err.exit().code(), err.retryable),
            (ErrorCode::ArchiveTooLarge, 4, false)
        );
        assert_eq!(err.details["limit"], 1 << 20);
        assert_eq!(err.details["config_key"], "limits.download_max_bytes");
        assert!(err.message.contains("codeberg.org/o/r"), "{}", err.message);
        assert!(started.elapsed() < Duration::from_secs(20));
        // The writer died with the group: the file stops growing.
        let size = fs::metadata(&grown).unwrap().len();
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(fs::metadata(&grown).unwrap().len(), size);
    }

    /// Output past the bound is read and dropped: what is kept stops at the bound, and
    /// git is never blocked on a full pipe.
    #[test]
    fn captured_output_is_bounded_and_drained() {
        let dir = tempfile::tempdir().unwrap();
        let mut git = runner(ProtocolPolicy::Https, &[]);
        git.program = stand_in(
            dir.path(),
            "head -c 300000 /dev/zero | tr '\\0' x >&2\necho done\n",
        );
        let (status, out) = finish(
            git.command(&[]),
            Duration::from_secs(10),
            "fetch",
            1000,
            None,
        )
        .unwrap();
        assert!(status.success());
        assert_eq!(out.stderr.len(), 1000);
        assert_eq!(out.stdout, "done\n");
    }

    /// A local repository: `main` at A, `dev` at B, annotated tag `v1` on B.
    struct Local {
        _dir: tempfile::TempDir,
        url: String,
        main: String,
        dev: String,
    }

    fn sh(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    fn local() -> Local {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("Addon");
        fs::create_dir_all(&work).unwrap();
        sh(&work, &["init", "--quiet", "--initial-branch=main"]);
        fs::write(work.join("Addon.toc"), "## Interface: 120105\n").unwrap();
        sh(&work, &["add", "."]);
        sh(&work, &["commit", "--quiet", "-m", "one"]);
        let main = sh(&work, &["rev-parse", "HEAD"]);
        sh(&work, &["checkout", "--quiet", "-b", "dev"]);
        fs::write(work.join("Addon.lua"), "-- dev\n").unwrap();
        sh(&work, &["add", "."]);
        sh(&work, &["commit", "--quiet", "-m", "two"]);
        let dev = sh(&work, &["rev-parse", "HEAD"]);
        sh(&work, &["tag", "-a", "v1", "-m", "v1"]);
        sh(&work, &["checkout", "--quiet", "main"]);
        let bare = dir.path().join("Addon.git");
        sh(
            dir.path(),
            &[
                "clone",
                "--quiet",
                "--bare",
                work.to_str().unwrap(),
                bare.to_str().unwrap(),
            ],
        );
        sh(&bare, &["config", "uploadpack.allowAnySHA1InWant", "true"]);
        Local {
            url: format!("file://{}", bare.display()),
            _dir: dir,
            main,
            dev,
        }
    }

    /// A local repository whose `main` holds a 2 MB file: (its dir, its URL, main's sha).
    fn big_local() -> (tempfile::TempDir, String, String) {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("Big");
        fs::create_dir_all(&work).unwrap();
        sh(&work, &["init", "--quiet", "--initial-branch=main"]);
        fs::write(work.join("Big.toc"), "## Interface: 120105\n").unwrap();
        fs::write(work.join("data.bin"), vec![0u8; 2 << 20]).unwrap();
        sh(&work, &["add", "."]);
        sh(&work, &["commit", "--quiet", "-m", "big"]);
        let sha = sh(&work, &["rev-parse", "HEAD"]);
        let bare = dir.path().join("Big.git");
        let (from, to) = (work.to_str().unwrap(), bare.to_str().unwrap());
        sh(dir.path(), &["clone", "--quiet", "--bare", from, to]);
        sh(&bare, &["config", "uploadpack.allowAnySHA1InWant", "true"]);
        let url = format!("file://{}", bare.display());
        (dir, url, sha)
    }

    #[test]
    fn a_checkout_past_the_download_limit_is_refused() {
        let (_repo, url, sha) = big_local();
        let out = tempfile::tempdir().unwrap();
        for (cap, fits) in [(1u64 << 20, false), (4 << 20, true)] {
            let git = runner(ProtocolPolicy::FileForTests, &[]).with_download_max_bytes(cap);
            // A branch clones; a sha fetches into a fresh repository.
            for rev in ["main", sha.as_str()] {
                let dest = out.path().join(format!("{cap}-{}", &rev[..4]));
                fs::create_dir_all(&dest).unwrap();
                let result = git.materialize(&url, rev, &sha, &dest);
                if fits {
                    result.unwrap();
                    assert!(dest.join("data.bin").is_file(), "{rev}");
                } else {
                    let err = result.unwrap_err();
                    assert_eq!(
                        err.code,
                        ErrorCode::ArchiveTooLarge,
                        "{rev}: {}",
                        err.message
                    );
                    assert_eq!(err.details["limit"], cap, "{rev}");
                    assert_eq!(err.details["config_key"], "limits.download_max_bytes");
                }
            }
        }
    }

    fn wanted(r: &str) -> Wanted {
        Wanted::stable(Ref::Git {
            url: r.split_once('#').map_or(r, |(u, _)| u).to_owned(),
            rev: r.split_once('#').map(|(_, rev)| rev.to_owned()),
        })
    }

    #[test]
    fn resolution_against_a_local_repository() {
        let repo = local();
        let git = runner(ProtocolPolicy::FileForTests, &[]);
        let release = |r: &str| git.fetch(&wanted(r)).map(|s| s.releases[0].clone());
        let head = release(&repo.url).unwrap();
        assert_eq!(head.release_id, format!("git:{}@{}", repo.url, repo.main));
        assert_eq!(head.version, repo.main[..7]);
        let dev = release(&format!("{}#dev", repo.url)).unwrap();
        assert_eq!(dev.release_id, format!("git:{}@{}", repo.url, repo.dev));
        let tag = release(&format!("{}#v1", repo.url)).unwrap();
        assert_eq!(
            (tag.version.as_str(), tag.release_id.ends_with(&repo.dev)),
            ("v1", true)
        );
        let err = release(&format!("{}#nope", repo.url)).unwrap_err();
        assert_eq!(err.code, ErrorCode::NotFound);
        let sha = release(&format!("{}#{}", repo.url, repo.dev)).unwrap();
        assert_eq!(
            sha.assets[0].location,
            AssetLocation::GitCheckout {
                url: repo.url.clone(),
                rev: repo.dev.clone(),
                sha: repo.dev.clone()
            }
        );
    }

    #[test]
    fn the_engine_installs_a_checkout_and_removes_it_after_the_commit() {
        let repo = local();
        let mut scene = crate::engine::testing::TxScene::new();
        scene.with_git = true;
        let change = crate::engine::plan::RequestedChange::Install {
            r#ref: Ref::Git {
                url: repo.url.clone(),
                rev: Some("dev".into()),
            },
            channel: Channel::Stable,
            channel_explicit: false,
            version: None,
            file: None,
            name: None,
            pin: crate::engine::plan::PinIntent::Keep,
            reason: crate::model::AddonReason::Explicit,
            reinstall: false,
            blob_sha256: None,
            asset: None,
        };
        let done = scene.run(vec![change]).unwrap();
        assert!(done.failures.is_empty(), "{:?}", done.failures);
        let outcome = &done.outcomes[0];
        let addon = scene.client.addons().join("Addon");
        assert!(
            addon.join("Addon.toc").is_file() && addon.join("Addon.lua").is_file(),
            "{outcome:?}"
        );
        assert!(
            !addon.join(".git").exists(),
            "the checkout's .git is not installed"
        );
        let tx = outcome.tx.as_ref().unwrap();
        assert!(
            !scene.paths.cache_tmp(tx).exists(),
            "the checkout went with the transaction's temp dir"
        );
    }

    /// An install whose checkout is over the limit fails that candidate with
    /// `archive_too_large` (the item: no candidate loads), and its partial checkout goes
    /// with the transaction's temp dir.
    #[test]
    fn the_engine_refuses_a_checkout_past_the_limit_and_leaves_nothing() {
        let (_repo, url, _) = big_local();
        let mut scene = crate::engine::testing::TxScene::new();
        scene.with_git = true;
        scene.git_download_max_bytes = 1 << 20;
        let change = crate::engine::plan::RequestedChange::Install {
            r#ref: Ref::Git {
                url,
                rev: Some("main".into()),
            },
            channel: Channel::Stable,
            channel_explicit: false,
            version: None,
            file: None,
            name: None,
            pin: crate::engine::plan::PinIntent::Keep,
            reason: crate::model::AddonReason::Explicit,
            reinstall: false,
            blob_sha256: None,
            asset: None,
        };
        let done = scene.run(vec![change]).unwrap();
        assert_eq!(done.failures.len(), 1, "{:?}", done.failures);
        let failure = &done.failures[0];
        assert_eq!(failure.error.code, ErrorCode::NoCompatibleRelease);
        let reason = failure.error.details["tried"][0]["reason"]
            .as_str()
            .unwrap();
        assert!(reason.starts_with("archive_too_large: "), "{reason}");
        assert!(done.outcomes.iter().all(|o| o.tx.is_none()));
        assert!(!scene.client.addons().join("Big").exists());
        let tmp = scene.paths.cache_dir().join("tmp");
        let left: Vec<_> = fs::read_dir(&tmp)
            .map(|d| d.flatten().map(|e| e.path()).collect())
            .unwrap_or_default();
        assert!(left.is_empty(), "{left:?}");
    }

    #[test]
    fn materialize_branch_tag_and_sha_and_a_moved_branch() {
        let repo = local();
        let git = runner(ProtocolPolicy::FileForTests, &[]);
        let out = tempfile::tempdir().unwrap();
        let fresh = |name: &str| {
            let d = out.path().join(name);
            fs::create_dir_all(&d).unwrap();
            d
        };
        let dest = fresh("branch");
        git.materialize(&repo.url, "dev", &repo.dev, &dest).unwrap();
        assert!(dest.join("Addon.lua").is_file() && dest.join("Addon.toc").is_file());
        let dest = fresh("tag");
        git.materialize(&repo.url, "v1", &repo.dev, &dest).unwrap();
        assert!(dest.join("Addon.lua").is_file());
        let dest = fresh("sha");
        git.materialize(&repo.url, &repo.dev, &repo.dev, &dest)
            .unwrap();
        assert!(dest.join("Addon.lua").is_file());
        // main resolved earlier, but the clone finds a different commit.
        let dest = fresh("moved");
        let err = git
            .materialize(&repo.url, "main", &repo.dev, &dest)
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::PlanStale);
        assert!(err.retryable);
    }
}
