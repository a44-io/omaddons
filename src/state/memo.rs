//! The release memo, `<cache>/omaddons/releases.json` (M2-D1): what each fetch learned,
//! so that a later run inside `SOURCE_RECHECK_MIN_INTERVAL` answers without a request, an
//! offline run still answers, and `outdated` followed by `update` fetches once.
//!
//! It is derived data, like the rest of the cache: written through fsx with no lock and
//! merged on write (two writers at once can lose one entry, which costs one request
//! later), and it can never fail a command. A missing, corrupt or newer file reads as
//! empty, a corrupt one is replaced by the next write, and a newer one is never
//! overwritten.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use jiff::Timestamp;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::doorbell;
use super::paths::Paths;
use super::store::{self, SchemaVersion, VersionStatus, Versioned};
use crate::error::{Error, ErrorCode, Result, Suggestion};
use crate::fsx::{Content, Op, ScopeTable, WriteSession, Writer, ensure_dir_all, mutate};
use crate::limits::MEMO_KNOWN_RELEASES;
use crate::model::{Actor, Channel, Ref, Release, ReleaseSet, Target};
use crate::resolve::select::newest;

/// The memo.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReleaseMemo {
    /// By [`key`]: the ref's identity and the channel.
    #[serde(default)]
    pub entries: BTreeMap<String, MemoEntry>,
    /// What downloading candidates taught: hashes, and whether they load on a client.
    #[serde(default)]
    pub candidates: Vec<CandidateFact>,
    /// The Tukui index, for dependencies and search.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tukui_list: Option<TukuiList>,
    /// What GitHub look-backs found, by [`look_back_key`]: believed while the newest
    /// release is the one they were made against.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub look_backs: BTreeMap<String, LookBackMemo>,
}

/// One look-back's answer for a ref, channel and client family.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LookBackMemo {
    /// The newest release's tag it was made against.
    pub newest: String,
    #[serde(flatten)]
    pub look_back: crate::sources::LookBack,
    #[schemars(schema_with = "crate::contract::schema::timestamp")]
    pub at: Timestamp,
}

/// The memo key of a look-back for `family`'s clients.
pub fn look_back_key(r: &Ref, channel: Channel, family: crate::rules::Family) -> String {
    format!("{}|{}", key(r, channel), family.as_str())
}

impl ReleaseMemo {
    /// The look-back remembered for `family`'s clients, made against `newest` (a tag).
    pub fn look_back(
        &self,
        r: &Ref,
        channel: Channel,
        family: crate::rules::Family,
        newest: &str,
    ) -> Option<crate::sources::LookBack> {
        self.look_backs
            .get(&look_back_key(r, channel, family))
            .filter(|m| m.newest == newest)
            .map(|m| m.look_back.clone())
    }
}

impl Versioned for ReleaseMemo {
    const KIND: &'static str = "release_memo";
    /// 1.1: each verdict records the judge it was made under (`judge`), and GitHub
    /// look-backs are remembered (`look_backs`). A 1.0 reader ignores both.
    const CURRENT: SchemaVersion = SchemaVersion::new(1, 1);
}

/// What one fetch of one ref on one channel learned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MemoEntry {
    pub wanted: MemoWanted,
    #[schemars(schema_with = "crate::contract::schema::timestamp")]
    pub fetched_at: Timestamp,
    /// The source's cheap re-check token: the newest release id, until a source offers a
    /// cheaper probe (a GitHub tag, a git sha).
    pub probe: Option<String>,
    /// The newest release the channel admits (no release when it admits none); `None`
    /// with a failure.
    pub set: Option<ReleaseSet>,
    /// The other releases the fetch saw, newest first, at most `MEMO_KNOWN_RELEASES`.
    #[serde(default)]
    pub known: Vec<KnownRelease>,
    /// A failure retrying does not change (`not_found`, `ref_invalid`).
    #[serde(default)]
    pub failure: Option<MemoFailure>,
}

/// The ref and channel an entry answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MemoWanted {
    #[serde(rename = "ref")]
    pub r#ref: Ref,
    pub channel: Channel,
}

/// A release seen beside the newest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct KnownRelease {
    pub release_id: String,
    #[schemars(schema_with = "crate::contract::schema::timestamp_opt")]
    pub published_at: Option<Timestamp>,
}

/// A remembered failure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MemoFailure {
    pub code: ErrorCode,
    pub message: String,
    #[schemars(schema_with = "crate::contract::schema::timestamp")]
    pub at: Timestamp,
}

/// What downloading one asset of one release taught: its hash and size, so the same
/// asset is never downloaded twice while its blob is cached, and whether it loads on a
/// client's rules.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CandidateFact {
    pub release_id: String,
    /// The asset's file name.
    pub asset: String,
    /// Lowercase hex.
    pub sha256: String,
    pub size: u64,
    pub verdicts: Vec<VerdictFact>,
    /// The download was not checked against the hash its source normally publishes
    /// (`download_unverified`), so a reuse of its blob is not either.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unverified: bool,
}

/// Whether a candidate loads on one rules row at one interface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VerdictFact {
    /// The client-rules row, e.g. `mainline`.
    pub rules: String,
    pub interface: u32,
    pub loads: bool,
    #[serde(default)]
    pub out_of_date: bool,
    /// The interface the addon's own folder declares.
    #[serde(default)]
    pub primary_interface: Option<u32>,
    #[schemars(schema_with = "crate::contract::schema::timestamp")]
    pub at: Timestamp,
    /// The [`crate::toc::judge_digest`] it was judged under: believed only while the
    /// client's is the same. `None` (written before 1.1) is another judge's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub judge: Option<String>,
}

/// The Tukui index: version metadata only, never URLs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TukuiList {
    #[schemars(schema_with = "crate::contract::schema::timestamp")]
    pub fetched_at: Timestamp,
    pub entries: Vec<TukuiEntry>,
}

/// One Tukui addon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TukuiEntry {
    pub id: i64,
    pub slug: String,
    pub name: String,
    pub author: String,
    pub version: String,
    /// The client patches it supports.
    pub patch: Vec<String>,
    pub directories: Vec<String>,
    pub last_update: String,
}

/// The memo key of `r` on `channel`: the ref's identity (GitHub owner and repo
/// lowercased, as refs compare), `|`, the channel.
pub fn key(r: &Ref, channel: Channel) -> String {
    let channel = match channel {
        Channel::Stable => "stable",
        Channel::Beta => "beta",
        Channel::Alpha => "alpha",
    };
    format!("{}|{channel}", r.identity())
}

/// Whether a failure is remembered: only the ones retrying does not change. A network
/// error, a 5xx, a rate limit or a disabled source is never stored.
pub fn remembered(code: ErrorCode) -> bool {
    matches!(code, ErrorCode::NotFound | ErrorCode::RefInvalid)
}

impl MemoEntry {
    /// The entry for a fetch of `r` on `channel` that returned `result` at `at`: the
    /// newest release the channel admits and up to `MEMO_KNOWN_RELEASES` others, or a
    /// remembered failure. `None` when the result is not remembered.
    pub fn of(
        r: &Ref,
        channel: Channel,
        result: &Result<ReleaseSet>,
        at: Timestamp,
    ) -> Option<Self> {
        let wanted = MemoWanted {
            r#ref: r.clone(),
            channel,
        };
        match result {
            Ok(set) => {
                let top = newest(set, channel).cloned();
                let mut others: Vec<&Release> = set
                    .releases
                    .iter()
                    .filter(|release| {
                        top.as_ref()
                            .is_none_or(|t| t.release_id != release.release_id)
                    })
                    .collect();
                // Sources list newest first; dates, when every release has one, decide.
                if others.iter().all(|release| release.published_at.is_some()) {
                    others.sort_by_key(|release| std::cmp::Reverse(release.published_at));
                }
                let known = others
                    .into_iter()
                    .take(MEMO_KNOWN_RELEASES)
                    .map(|release| KnownRelease {
                        release_id: release.release_id.clone(),
                        published_at: release.published_at,
                    })
                    .collect();
                Some(Self {
                    wanted,
                    fetched_at: at,
                    probe: top.as_ref().map(|t| t.release_id.clone()),
                    set: Some(ReleaseSet {
                        r#ref: set.r#ref.clone(),
                        releases: top.into_iter().collect(),
                    }),
                    known,
                    failure: None,
                })
            }
            Err(error) if remembered(error.code) => Some(Self {
                wanted,
                fetched_at: at,
                probe: None,
                set: None,
                known: Vec::new(),
                failure: Some(MemoFailure {
                    code: error.code,
                    message: error.message.clone(),
                    at,
                }),
            }),
            Err(_) => None,
        }
    }

    /// The fetch result this entry stands for. A remembered failure's fix re-checks the
    /// ref.
    pub fn result(&self) -> Result<ReleaseSet> {
        match (&self.set, &self.failure) {
            (Some(set), _) => Ok(set.clone()),
            (None, Some(failure)) => Err(Error::new(
                failure.code,
                failure.message.clone(),
                Suggestion::command([
                    "omaddons".to_owned(),
                    "info".to_owned(),
                    self.wanted.r#ref.to_string(),
                    "--refresh".to_owned(),
                ]),
            )),
            (None, None) => Ok(ReleaseSet {
                r#ref: self.wanted.r#ref.clone(),
                releases: Vec::new(),
            }),
        }
    }

    /// Fetched less than `window` before `now`. A `fetched_at` in the future (a clock
    /// that moved back) is stale.
    pub fn fresh(&self, now: Timestamp, window: Duration) -> bool {
        fresh(self.fetched_at, now, window)
    }
}

impl TukuiList {
    /// Fetched less than `window` before `now`, as [`MemoEntry::fresh`].
    pub fn fresh(&self, now: Timestamp, window: Duration) -> bool {
        fresh(self.fetched_at, now, window)
    }
}

/// `fetched_at` is less than `window` before `now`, and not in the future.
fn fresh(fetched_at: Timestamp, now: Timestamp, window: Duration) -> bool {
    let Ok(window) = jiff::SignedDuration::try_from(window) else {
        return false;
    };
    fetched_at <= now && now.duration_since(fetched_at) < window
}

impl CandidateFact {
    /// When the newest of its verdicts was learned.
    fn learned(&self) -> Option<Timestamp> {
        self.verdicts.iter().map(|v| v.at).max()
    }
}

/// Adds `fact` to `facts`, one fact per release and asset: the hash and size of the one
/// added, the later verdict per rules row and interface, and `unverified` once either
/// was.
pub fn merge_fact(facts: &mut Vec<CandidateFact>, fact: CandidateFact) {
    let same = facts
        .iter_mut()
        .find(|f| f.release_id == fact.release_id && f.asset == fact.asset);
    let Some(kept) = same else {
        facts.push(fact);
        return;
    };
    kept.sha256 = fact.sha256;
    kept.size = fact.size;
    // Once unchecked, never claimed checked.
    kept.unverified |= fact.unverified;
    for verdict in fact.verdicts {
        let at = kept
            .verdicts
            .iter_mut()
            .find(|v| v.rules == verdict.rules && v.interface == verdict.interface);
        match at {
            Some(old) if old.at > verdict.at => {}
            Some(old) => *old = verdict,
            None => kept.verdicts.push(verdict),
        }
    }
}

/// Lookups over candidate facts: the hash of a release's asset, and whether it loads on
/// a rules row at an interface.
#[derive(Debug, Clone, Default)]
pub struct FactIndex {
    hashes: BTreeMap<(String, String), (String, u64)>,
    verdicts: BTreeMap<(String, String, String, u32), VerdictFact>,
    /// The assets whose download went unchecked.
    unverified: BTreeSet<(String, String)>,
}

impl FactIndex {
    pub fn new(facts: &[CandidateFact]) -> Self {
        let mut index = Self::default();
        index.extend(facts);
        index
    }

    /// Adds `facts`, which win over what the index knew (they are newer).
    pub fn extend(&mut self, facts: &[CandidateFact]) {
        for fact in facts {
            let asset = (fact.release_id.clone(), fact.asset.clone());
            self.hashes
                .insert(asset.clone(), (fact.sha256.clone(), fact.size));
            if fact.unverified {
                self.unverified.insert(asset.clone());
            }
            for verdict in &fact.verdicts {
                let key = (
                    asset.0.clone(),
                    asset.1.clone(),
                    verdict.rules.clone(),
                    verdict.interface,
                );
                self.verdicts.insert(key, verdict.clone());
            }
        }
    }

    /// Whether the download of `asset` of `release_id` went unchecked.
    pub fn unverified(&self, release_id: &str, asset: &str) -> bool {
        self.unverified
            .contains(&(release_id.to_owned(), asset.to_owned()))
    }

    /// Whether the archive with this SHA-256 is an unchecked download of `release_id`
    /// (whatever its asset was named): a cached blob reused as it is.
    pub fn unverified_blob(&self, release_id: &str, sha256: &str) -> bool {
        self.unverified.iter().any(|key| {
            key.0 == release_id
                && self
                    .hashes
                    .get(key)
                    .is_some_and(|(sha, _)| sha.eq_ignore_ascii_case(sha256))
        })
    }

    /// The SHA-256 (lowercase hex) and size of `asset` of `release_id`, when downloaded.
    pub fn hash(&self, release_id: &str, asset: &str) -> Option<(&str, u64)> {
        self.hashes
            .get(&(release_id.to_owned(), asset.to_owned()))
            .map(|(sha256, size)| (sha256.as_str(), *size))
    }

    /// The release an archive with this SHA-256 was downloaded as, when one was.
    pub fn release_of(&self, sha256: &str) -> Option<&str> {
        self.hashes
            .iter()
            .find(|(_, (sha, _))| sha.eq_ignore_ascii_case(sha256))
            .map(|((release_id, _), _)| release_id.as_str())
    }

    /// Whether `asset` of `release_id` loads on `target` (its rules row and interface),
    /// when a plan verified it there under the judge `target` has now: a verdict an
    /// older judge made, or another `[client_rules]` patch, says nothing.
    pub fn verdict(&self, release_id: &str, asset: &str, target: &Target) -> Option<&VerdictFact> {
        let key = (
            release_id.to_owned(),
            asset.to_owned(),
            target.rules.name.clone(),
            target.interface,
        );
        self.verdicts
            .get(&key)
            .filter(|v| v.judge.as_deref() == Some(target.judge.as_str()))
    }
}

impl ReleaseMemo {
    /// Merges `newer` in: an entry fetched later wins its key; candidate facts union by
    /// release and asset, the later verdict winning per rules row and interface; the
    /// Tukui list fetched later wins.
    pub fn merge(&mut self, newer: Self) {
        for (key, entry) in newer.entries {
            match self.entries.get(&key) {
                Some(kept) if kept.fetched_at > entry.fetched_at => {}
                _ => {
                    self.entries.insert(key, entry);
                }
            }
        }
        for fact in newer.candidates {
            merge_fact(&mut self.candidates, fact);
        }
        if let Some(list) = newer.tukui_list
            && self
                .tukui_list
                .as_ref()
                .is_none_or(|kept| kept.fetched_at <= list.fetched_at)
        {
            self.tukui_list = Some(list);
        }
        for (key, look_back) in newer.look_backs {
            match self.look_backs.get(&key) {
                Some(kept) if kept.at > look_back.at => {}
                _ => {
                    self.look_backs.insert(key, look_back);
                }
            }
        }
    }

    /// Keeps at most `max` entries and `max` candidate facts, dropping the oldest.
    pub fn cap(&mut self, max: usize) {
        if self.entries.len() > max {
            let mut by_age: Vec<(Timestamp, String)> = self
                .entries
                .iter()
                .map(|(key, e)| (e.fetched_at, key.clone()))
                .collect();
            by_age.sort();
            let excess = self.entries.len() - max;
            for (_, key) in by_age.into_iter().take(excess) {
                self.entries.remove(&key);
            }
        }
        if self.candidates.len() > max {
            self.candidates
                .sort_by_key(|f| std::cmp::Reverse(f.learned()));
            self.candidates.truncate(max);
        }
        if self.look_backs.len() > max {
            let mut by_age: Vec<(Timestamp, String)> = self
                .look_backs
                .iter()
                .map(|(key, l)| (l.at, key.clone()))
                .collect();
            by_age.sort();
            let excess = self.look_backs.len() - max;
            for (_, key) in by_age.into_iter().take(excess) {
                self.look_backs.remove(&key);
            }
        }
    }
}

/// The memo on disk; empty when it is missing, corrupt or written by a newer omaddons,
/// the last two with a `-v` note.
pub fn load(paths: &Paths) -> (ReleaseMemo, Option<String>) {
    let path = paths.release_memo();
    match store::load::<ReleaseMemo>(&path) {
        Ok(None) => (ReleaseMemo::default(), None),
        Ok(Some(loaded)) if loaded.status == VersionStatus::Newer => (
            ReleaseMemo::default(),
            Some(format!(
                "release memo: {} was written by a newer omaddons; not used",
                path.display()
            )),
        ),
        Ok(Some(loaded)) => (loaded.value, None),
        Err(error) => (
            ReleaseMemo::default(),
            Some(format!("release memo ignored: {}", error.message)),
        ),
    }
}

/// Merges `update` into the memo on disk and writes it, capped at `max` entries, then
/// rings the doorbell for `command` run by `actor`: a changed memo can change what
/// `status` reports (its update counts and `checked_at`). A merge that leaves the file
/// as it was writes and rings nothing. Best effort: `Some(note)` for `-v` when the
/// write was skipped (a newer file) or failed. A corrupt file is replaced.
pub fn write(
    paths: &Paths,
    update: ReleaseMemo,
    max: usize,
    command: &str,
    actor: Actor,
) -> Option<String> {
    let path = paths.release_memo();
    // Read once: merged into, and compared with what the merge would write.
    let on_disk = std::fs::read(&path).ok();
    let loaded = on_disk
        .as_deref()
        .map(|bytes| store::load_bytes::<ReleaseMemo>(&path, bytes));
    let mut memo = match loaded {
        Some(Ok(loaded)) if loaded.status == VersionStatus::Newer => {
            return Some(format!(
                "release memo: {} was written by a newer omaddons; not updated",
                path.display()
            ));
        }
        Some(Ok(loaded)) => loaded.value,
        Some(Err(_)) | None => ReleaseMemo::default(),
    };
    memo.merge(update);
    memo.cap(max);
    let written = (|| -> Result<bool> {
        let json = store::encode(&memo)?;
        if on_disk.as_deref() == Some(json.as_slice()) {
            return Ok(false);
        }
        let scopes = ScopeTable::new(paths, &[], None)?;
        let mut s = WriteSession::untracked(Writer::Derived, &scopes, "release-memo");
        ensure_dir_all(&mut s, paths.cache_dir())?;
        let op = Op::WriteFile {
            content: Content::Bytes(&json),
            mode: Some(0o644),
        };
        mutate(&mut s, &path, op)?;
        Ok(true)
    })();
    match written {
        Ok(true) => {
            doorbell::ring(paths, command, actor);
            None
        }
        Ok(false) => None,
        Err(error) => Some(format!("release memo not written: {}", error.message)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    fn release(tag: &str, channel: Channel, published: Option<&str>) -> Release {
        Release {
            release_id: format!("gh:o/foo@{tag}"),
            version: tag.to_owned(),
            channel,
            published_at: published.map(at),
            assets: Vec::new(),
            changelog_url: None,
            notes: None,
            name_hint: None,
            revision: None,
        }
    }

    fn set(releases: Vec<Release>) -> ReleaseSet {
        ReleaseSet {
            r#ref: "gh:o/foo".parse().unwrap(),
            releases,
        }
    }

    fn entry(fetched_at: &str) -> MemoEntry {
        let r: Ref = "gh:o/foo".parse().unwrap();
        let found = Ok(set(vec![release("1", Channel::Stable, None)]));
        MemoEntry::of(&r, Channel::Stable, &found, at(fetched_at)).unwrap()
    }

    fn paths() -> (tempfile::TempDir, Paths) {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().to_path_buf();
        let paths = Paths::with_roots(&base, base.join("c"), base.join("s"), base.join("k"));
        (dir, paths)
    }

    #[test]
    fn keys_are_the_ref_identity_and_the_channel() {
        let upper: Ref = "gh:DBM/X".parse().unwrap();
        let lower: Ref = "gh:dbm/x".parse().unwrap();
        assert_eq!(key(&upper, Channel::Stable), key(&lower, Channel::Stable));
        assert_eq!(key(&upper, Channel::Stable), "gh:dbm/x|stable");
        assert_ne!(key(&upper, Channel::Stable), key(&upper, Channel::Beta));
    }

    #[test]
    fn an_entry_keeps_the_newest_eligible_release_and_up_to_ten_others() {
        let r: Ref = "gh:o/foo".parse().unwrap();
        let mut releases: Vec<Release> = (1..=13)
            .map(|n| {
                release(
                    &n.to_string(),
                    Channel::Stable,
                    Some(&format!("2026-10-{n:02}T00:00:00Z")),
                )
            })
            .collect();
        releases.push(release(
            "14-beta",
            Channel::Beta,
            Some("2026-10-14T00:00:00Z"),
        ));
        let e = MemoEntry::of(
            &r,
            Channel::Stable,
            &Ok(set(releases)),
            at("2026-10-15T00:00:00Z"),
        )
        .unwrap();
        let kept = e.set.as_ref().unwrap();
        assert_eq!(kept.releases.len(), 1);
        assert_eq!(kept.releases[0].version, "13", "the newest stable");
        assert_eq!(e.probe.as_deref(), Some("gh:o/foo@13"));
        let known: Vec<&str> = e.known.iter().map(|k| k.release_id.as_str()).collect();
        assert_eq!(known.len(), MEMO_KNOWN_RELEASES);
        assert_eq!(known[0], "gh:o/foo@14-beta", "newest first, any channel");
        assert_eq!(known[1], "gh:o/foo@12");
        assert!(e.failure.is_none());

        // No release the channel admits: an empty set, not a failure.
        let beta_only = Ok(set(vec![release("2", Channel::Beta, None)]));
        let e = MemoEntry::of(&r, Channel::Stable, &beta_only, at("2026-10-15T00:00:00Z")).unwrap();
        assert!(e.result().unwrap().releases.is_empty());
    }

    #[test]
    fn only_failures_retrying_cannot_change_are_remembered() {
        let r: Ref = "gh:o/foo".parse().unwrap();
        let now = at("2026-10-15T00:00:00Z");
        let failed = |code| Err(Error::new(code, "x", Suggestion::text("y")));
        for code in [ErrorCode::NotFound, ErrorCode::RefInvalid] {
            let e = MemoEntry::of(&r, Channel::Stable, &failed(code), now).unwrap();
            assert!(e.set.is_none());
            let error = e.result().unwrap_err();
            assert_eq!(error.code, code);
            assert_eq!(
                error.fix.argv().map(<[String]>::to_vec),
                Some(
                    ["omaddons", "info", "gh:o/foo", "--refresh"]
                        .map(String::from)
                        .to_vec()
                )
            );
        }
        for code in [
            ErrorCode::Network,
            ErrorCode::HttpStatus,
            ErrorCode::RateLimited,
            ErrorCode::SourceDisabled,
            ErrorCode::Offline,
        ] {
            assert!(
                MemoEntry::of(&r, Channel::Stable, &failed(code), now).is_none(),
                "{code}"
            );
        }
    }

    #[test]
    fn freshness_is_a_window_and_the_future_is_stale() {
        let e = entry("2026-10-15T12:00:00Z");
        let window = Duration::from_secs(15 * 60);
        assert!(e.fresh(at("2026-10-15T12:14:59Z"), window));
        assert!(!e.fresh(at("2026-10-15T12:15:00Z"), window));
        assert!(
            !e.fresh(at("2026-10-15T11:59:00Z"), window),
            "fetched in the future: a clock that moved back"
        );
    }

    #[test]
    fn merging_keeps_the_newer_of_everything() {
        let mut memo = ReleaseMemo::default();
        memo.entries
            .insert("a".into(), entry("2026-10-15T12:00:00Z"));
        memo.entries
            .insert("b".into(), entry("2026-10-15T12:00:00Z"));
        let verdict = |rules: &str, loads, when| VerdictFact {
            rules: rules.into(),
            interface: 120_105,
            loads,
            out_of_date: false,
            primary_interface: None,
            at: at(when),
            judge: None,
        };
        let fact = |verdicts| CandidateFact {
            release_id: "gh:o/foo@1".into(),
            asset: "Foo.zip".into(),
            sha256: "aa".into(),
            size: 1,
            verdicts,
            unverified: false,
        };
        memo.candidates.push(fact(vec![
            verdict("mainline", true, "2026-10-15T12:00:00Z"),
            verdict("mists", false, "2026-10-15T12:00:00Z"),
        ]));
        let list = |when: &str| TukuiList {
            fetched_at: at(when),
            entries: Vec::new(),
        };
        memo.tukui_list = Some(list("2026-10-15T12:00:00Z"));

        let mut newer = ReleaseMemo::default();
        newer
            .entries
            .insert("a".into(), entry("2026-10-15T13:00:00Z"));
        newer
            .entries
            .insert("b".into(), entry("2026-10-15T11:00:00Z"));
        newer
            .entries
            .insert("c".into(), entry("2026-10-15T11:00:00Z"));
        newer.candidates.push(fact(vec![
            verdict("mainline", false, "2026-10-15T13:00:00Z"),
            verdict("mists", true, "2026-10-15T11:00:00Z"),
            verdict("vanilla", true, "2026-10-15T11:00:00Z"),
        ]));
        newer.tukui_list = Some(list("2026-10-15T11:00:00Z"));
        memo.merge(newer);

        let fetched = |k: &str| memo.entries[k].fetched_at;
        assert_eq!(fetched("a"), at("2026-10-15T13:00:00Z"), "the newer wins");
        assert_eq!(fetched("b"), at("2026-10-15T12:00:00Z"), "the older loses");
        assert!(memo.entries.contains_key("c"));
        assert_eq!(memo.candidates.len(), 1, "one fact per release and asset");
        let loads: Vec<(&str, bool)> = memo.candidates[0]
            .verdicts
            .iter()
            .map(|v| (v.rules.as_str(), v.loads))
            .collect();
        assert_eq!(
            loads,
            [("mainline", false), ("mists", false), ("vanilla", true)],
            "the newer verdict per rules row and interface"
        );
        assert_eq!(
            memo.tukui_list.as_ref().unwrap().fetched_at,
            at("2026-10-15T12:00:00Z")
        );
    }

    /// Retail 12.1.5, judged by the builtin rules.
    fn retail() -> Target {
        crate::engine::testing::target(std::path::Path::new("/nonexistent/_retail_"))
    }

    #[test]
    fn facts_merge_per_asset_and_the_index_finds_hashes_and_verdicts() {
        let retail = retail();
        let verdict = |rules: &str, interface, loads, when| VerdictFact {
            rules: rules.into(),
            interface,
            loads,
            out_of_date: false,
            primary_interface: Some(interface),
            at: at(when),
            judge: Some(retail.judge.clone()),
        };
        let mainline = retail.rules.name.as_str();
        let fact = |asset: &str, sha: &str, verdicts| CandidateFact {
            release_id: "gh:o/foo@1".into(),
            asset: asset.into(),
            sha256: sha.into(),
            size: 7,
            verdicts,
            unverified: false,
        };
        let mut facts = Vec::new();
        merge_fact(
            &mut facts,
            fact(
                "Foo.zip",
                "aa",
                vec![verdict(mainline, 120_105, true, "2026-10-15T10:00:00Z")],
            ),
        );
        merge_fact(
            &mut facts,
            fact(
                "Foo.zip",
                "bb",
                vec![
                    verdict(mainline, 120_105, false, "2026-10-15T11:00:00Z"),
                    verdict(mainline, 120_200, true, "2026-10-15T11:00:00Z"),
                ],
            ),
        );
        merge_fact(&mut facts, fact("Foo-nolib.zip", "cc", Vec::new()));
        assert_eq!(facts.len(), 2, "one fact per release and asset");
        assert_eq!(
            facts[0].verdicts.len(),
            2,
            "one verdict per rules row and interface"
        );

        let index = FactIndex::new(&facts);
        assert_eq!(index.hash("gh:o/foo@1", "Foo.zip"), Some(("bb", 7)));
        assert_eq!(index.hash("gh:o/foo@1", "Foo-nolib.zip"), Some(("cc", 7)));
        assert_eq!(index.hash("gh:o/foo@2", "Foo.zip"), None);
        let found = index.verdict("gh:o/foo@1", "Foo.zip", &retail);
        assert_eq!(found.map(|v| v.loads), Some(false), "the later verdict");
        let other = Target {
            interface: 120_000,
            ..retail.clone()
        };
        assert!(
            index.verdict("gh:o/foo@1", "Foo.zip", &other).is_none(),
            "another interface is another verdict"
        );

        // An unchecked download stays unchecked through a later merge.
        assert!(!index.unverified("gh:o/foo@1", "Foo.zip"));
        let unchecked = CandidateFact {
            unverified: true,
            ..fact("Foo.zip", "bb", Vec::new())
        };
        merge_fact(&mut facts, unchecked);
        merge_fact(&mut facts, fact("Foo.zip", "bb", Vec::new()));
        assert!(facts[0].unverified);
        let index = FactIndex::new(&facts);
        assert!(index.unverified("gh:o/foo@1", "Foo.zip"));
        // Found by its archive too, as a reinstall reuses it (any spelling of the hex).
        assert!(index.unverified_blob("gh:o/foo@1", "BB"));
        assert!(
            !index.unverified_blob("gh:o/foo@1", "cc"),
            "a checked asset"
        );
        assert!(
            !index.unverified_blob("gh:o/foo@2", "bb"),
            "another release"
        );
        // Written only when set; a fact from before the field loads as checked.
        let json = serde_json::to_value(&facts[1]).unwrap();
        assert!(json.get("unverified").is_none(), "{json}");
        let old: CandidateFact = serde_json::from_value(json).unwrap();
        assert!(!old.unverified);
        assert_eq!(
            serde_json::to_value(&facts[0]).unwrap()["unverified"],
            serde_json::json!(true)
        );
    }

    /// A verdict is believed only under the judge it was made under: one recorded by
    /// another judge (an older omaddons, another `[client_rules]` patch) or by none
    /// (before 1.1) says nothing, and stays until a new verdict replaces it.
    #[test]
    fn a_verdict_from_another_judge_is_not_believed() {
        let retail = retail();
        let fact = |judge: Option<&str>| CandidateFact {
            release_id: "gh:o/foo@2".into(),
            asset: "Foo.zip".into(),
            sha256: "aa".into(),
            size: 1,
            verdicts: vec![VerdictFact {
                rules: retail.rules.name.clone(),
                interface: retail.interface,
                loads: true,
                out_of_date: true,
                primary_interface: Some(110_207),
                at: at("2026-10-15T10:00:00Z"),
                judge: judge.map(str::to_owned),
            }],
            unverified: false,
        };
        let believed = |judge: Option<&str>| {
            FactIndex::new(&[fact(judge)])
                .verdict("gh:o/foo@2", "Foo.zip", &retail)
                .is_some()
        };
        assert!(believed(Some(&retail.judge)), "the current judge");
        assert!(!believed(None), "no judge recorded");
        assert!(!believed(Some("0".repeat(64).as_str())), "another judge");
        // The hash is still known whatever judged it: the download cache keeps it.
        assert_eq!(
            FactIndex::new(&[fact(None)]).hash("gh:o/foo@2", "Foo.zip"),
            Some(("aa", 1))
        );

        // A patch that changes how the row judges is another judge; one that changes
        // nothing judging reads (a product) is not.
        let floor: crate::rules::RulesOverrides =
            toml::from_str("[client_rules.mainline]\ninterface_floor = 0\n").unwrap();
        let table = crate::rules::RulesTable::builtin()
            .with_overrides(&floor)
            .unwrap();
        let mut patched = retail.clone();
        patched.refresh_rules(&table);
        assert_ne!(patched.judge, retail.judge);
        let product: crate::rules::RulesOverrides = toml::from_str(
            "[[products]]\ncode = \"wow_newflavor\"\nfolder = \"_newflavor_\"\nexe = \"WowNew.exe\"\nkind = \"live\"\ndefault_id = \"newflavor\"\n",
        )
        .unwrap();
        let table = crate::rules::RulesTable::builtin()
            .with_overrides(&product)
            .unwrap();
        let mut unrelated = retail.clone();
        unrelated.refresh_rules(&table);
        assert_eq!(unrelated.judge, retail.judge);
        // The digest is the version's too.
        assert_eq!(crate::toc::JUDGE_VERSION, 1);
    }

    #[test]
    fn the_cap_drops_the_oldest() {
        let mut memo = ReleaseMemo::default();
        for (k, when) in [
            ("a", "2026-10-15T10:00:00Z"),
            ("b", "2026-10-15T13:00:00Z"),
            ("c", "2026-10-15T11:00:00Z"),
            ("d", "2026-10-15T12:00:00Z"),
        ] {
            memo.entries.insert(k.into(), entry(when));
            memo.candidates.push(CandidateFact {
                release_id: k.into(),
                asset: "x.zip".into(),
                sha256: String::new(),
                size: 0,
                unverified: false,
                verdicts: vec![VerdictFact {
                    rules: "mainline".into(),
                    interface: 1,
                    loads: true,
                    out_of_date: false,
                    primary_interface: None,
                    at: at(when),
                    judge: None,
                }],
            });
        }
        memo.cap(2);
        assert_eq!(memo.entries.keys().collect::<Vec<_>>(), ["b", "d"]);
        let kept: Vec<&str> = memo
            .candidates
            .iter()
            .map(|f| f.release_id.as_str())
            .collect();
        assert_eq!(kept, ["b", "d"]);
    }

    fn look_back(newest: &str, found: &str, when: &str) -> LookBackMemo {
        LookBackMemo {
            newest: newest.into(),
            look_back: crate::sources::LookBack {
                found: Some(set(vec![release(found, Channel::Stable, None)])),
                ruled_out: vec![("v3".into(), "a prerelease".into())],
                order: vec![newest.into(), "v3".into(), found.into()],
            },
            at: at(when),
        }
    }

    #[test]
    fn look_backs_are_believed_against_their_newest_tag_and_the_later_wins() {
        use crate::rules::Family;
        let r: Ref = "gh:O/Foo".parse().unwrap();
        let k = look_back_key(&r, Channel::Stable, Family::Mists);
        assert_eq!(k, "gh:o/foo|stable|mists");
        assert_ne!(k, look_back_key(&r, Channel::Stable, Family::Tbc));
        let mut memo = ReleaseMemo::default();
        memo.look_backs
            .insert(k.clone(), look_back("v4", "v2", "2026-10-15T10:00:00Z"));
        let found = memo
            .look_back(&r, Channel::Stable, Family::Mists, "v4")
            .unwrap();
        assert_eq!(found.found.unwrap().releases[0].version, "v2");
        // A newer release asks again, and another family has its own.
        assert!(
            memo.look_back(&r, Channel::Stable, Family::Mists, "v5")
                .is_none()
        );
        assert!(
            memo.look_back(&r, Channel::Stable, Family::Tbc, "v4")
                .is_none()
        );

        let mut older = ReleaseMemo::default();
        older
            .look_backs
            .insert(k.clone(), look_back("v3", "v1", "2026-10-14T10:00:00Z"));
        memo.merge(older);
        assert_eq!(memo.look_backs[&k].newest, "v4");
        let mut later = ReleaseMemo::default();
        later
            .look_backs
            .insert(k.clone(), look_back("v5", "v2", "2026-10-16T10:00:00Z"));
        memo.merge(later);
        assert_eq!(memo.look_backs[&k].newest, "v5");

        // It round-trips, and the cap drops the oldest.
        let text = serde_json::to_string(&memo).unwrap();
        let back: ReleaseMemo = serde_json::from_str(&text).unwrap();
        assert_eq!(back.look_backs, memo.look_backs);
        for (family, when) in [
            (Family::Tbc, "2026-10-17T10:00:00Z"),
            (Family::Vanilla, "2026-10-13T10:00:00Z"),
        ] {
            memo.look_backs.insert(
                look_back_key(&r, Channel::Stable, family),
                look_back("v5", "v2", when),
            );
        }
        memo.cap(2);
        let kept: Vec<&str> = memo.look_backs.keys().map(String::as_str).collect();
        assert_eq!(kept, ["gh:o/foo|stable|mists", "gh:o/foo|stable|tbc"]);
    }

    #[test]
    fn reads_are_tolerant_and_writes_merge_with_the_file() {
        let (_dir, paths) = paths();
        assert_eq!(load(&paths), (ReleaseMemo::default(), None), "missing");

        let mut first = ReleaseMemo::default();
        first
            .entries
            .insert("a".into(), entry("2026-10-15T12:00:00Z"));
        assert_eq!(write(&paths, first, 10, "outdated", Actor::Human), None);
        let mut second = ReleaseMemo::default();
        second
            .entries
            .insert("b".into(), entry("2026-10-15T12:00:00Z"));
        assert_eq!(write(&paths, second, 10, "outdated", Actor::Human), None);
        let (memo, note) = load(&paths);
        assert_eq!(note, None);
        assert_eq!(
            memo.entries.keys().collect::<Vec<_>>(),
            ["a", "b"],
            "merged with what was there"
        );

        std::fs::write(paths.release_memo(), "{ not json").unwrap();
        let (memo, note) = load(&paths);
        assert!(memo.entries.is_empty());
        assert!(note.is_some_and(|n| n.contains("ignored")));
        let mut third = ReleaseMemo::default();
        third
            .entries
            .insert("c".into(), entry("2026-10-15T12:00:00Z"));
        assert_eq!(
            write(&paths, third, 10, "outdated", Actor::Human),
            None,
            "a corrupt file is replaced"
        );
        assert_eq!(load(&paths).0.entries.keys().collect::<Vec<_>>(), ["c"]);

        let newer = r#"{"schema_version": "9.0", "entries": {}}"#;
        std::fs::write(paths.release_memo(), newer).unwrap();
        let (memo, note) = load(&paths);
        assert!(memo.entries.is_empty() && note.is_some());
        let note = write(&paths, ReleaseMemo::default(), 10, "outdated", Actor::Human);
        assert!(note.is_some_and(|n| n.contains("newer")));
        assert_eq!(
            std::fs::read_to_string(paths.release_memo()).unwrap(),
            newer,
            "a newer memo is never overwritten"
        );
    }

    #[test]
    fn a_write_that_changes_the_memo_rings_and_one_that_does_not_writes_nothing() {
        use crate::state::doorbell::Doorbell;

        let (_dir, paths) = paths();
        let rung = || {
            store::load::<Doorbell>(&paths.doorbell_file())
                .unwrap()
                .map(|loaded| (loaded.value.command, loaded.value.actor))
        };
        let update = || {
            let mut memo = ReleaseMemo::default();
            memo.entries
                .insert("a".into(), entry("2026-10-15T12:00:00Z"));
            memo
        };
        assert_eq!(write(&paths, update(), 10, "outdated", Actor::Widget), None);
        assert_eq!(rung(), Some(("outdated".to_owned(), Actor::Widget)));
        let bytes = std::fs::read(paths.release_memo()).unwrap();

        std::fs::remove_file(paths.doorbell_file()).unwrap();
        assert_eq!(write(&paths, update(), 10, "info", Actor::Human), None);
        assert_eq!(rung(), None, "the same merge again changes nothing");
        assert_eq!(std::fs::read(paths.release_memo()).unwrap(), bytes);

        let mut later = ReleaseMemo::default();
        later
            .entries
            .insert("a".into(), entry("2026-10-15T12:30:00Z"));
        assert_eq!(write(&paths, later, 10, "install", Actor::Agent), None);
        assert_eq!(
            rung(),
            Some(("install".to_owned(), Actor::Agent)),
            "a newer fetch of the same ref changes the memo"
        );
    }
}
