//! The release memo, `<cache>/omaddons/releases.json` (M2-D1): what each fetch learned,
//! so that a later run inside `SOURCE_RECHECK_MIN_INTERVAL` answers without a request, an
//! offline run still answers, and `outdated` followed by `update` fetches once.
//!
//! It is derived data, like the rest of the cache: written through fsx with no lock and
//! merged on write (two writers at once can lose one entry, which costs one request
//! later), and it can never fail a command. A missing, corrupt or newer file reads as
//! empty, a corrupt one is replaced by the next write, and a newer one is never
//! overwritten.

use std::collections::BTreeMap;
use std::time::Duration;

use jiff::Timestamp;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::paths::Paths;
use super::store::{self, SchemaVersion, VersionStatus, Versioned};
use crate::error::{Error, ErrorCode, Result, Suggestion};
use crate::fsx::{Content, Op, ScopeTable, WriteSession, Writer, ensure_dir_all, mutate};
use crate::limits::MEMO_KNOWN_RELEASES;
use crate::model::{Channel, Ref, Release, ReleaseSet};
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
}

impl Versioned for ReleaseMemo {
    const KIND: &'static str = "release_memo";
    const CURRENT: SchemaVersion = SchemaVersion::new(1, 0);
}

/// What one fetch of one ref on one channel learned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MemoEntry {
    pub wanted: MemoWanted,
    #[schemars(with = "String")]
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
    #[schemars(with = "Option<String>")]
    pub published_at: Option<Timestamp>,
}

/// A remembered failure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MemoFailure {
    pub code: ErrorCode,
    pub message: String,
    #[schemars(with = "String")]
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
    #[schemars(with = "String")]
    pub at: Timestamp,
}

/// The Tukui index: version metadata only, never URLs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TukuiList {
    #[schemars(with = "String")]
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
        let Ok(window) = jiff::SignedDuration::try_from(window) else {
            return false;
        };
        self.fetched_at <= now && now.duration_since(self.fetched_at) < window
    }
}

impl CandidateFact {
    /// When the newest of its verdicts was learned.
    fn learned(&self) -> Option<Timestamp> {
        self.verdicts.iter().map(|v| v.at).max()
    }
}

/// Adds `fact` to `facts`, one fact per release and asset: the hash and size of the one
/// added, and the later verdict per rules row and interface.
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

    /// The SHA-256 (lowercase hex) and size of `asset` of `release_id`, when downloaded.
    pub fn hash(&self, release_id: &str, asset: &str) -> Option<(&str, u64)> {
        self.hashes
            .get(&(release_id.to_owned(), asset.to_owned()))
            .map(|(sha256, size)| (sha256.as_str(), *size))
    }

    /// Whether `asset` of `release_id` loads on the rules row `rules` at `interface`, when
    /// a plan verified it there.
    pub fn verdict(
        &self,
        release_id: &str,
        asset: &str,
        rules: &str,
        interface: u32,
    ) -> Option<&VerdictFact> {
        let key = (
            release_id.to_owned(),
            asset.to_owned(),
            rules.to_owned(),
            interface,
        );
        self.verdicts.get(&key)
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

/// Merges `update` into the memo on disk and writes it, capped at `max` entries.
/// Best effort: `Some(note)` for `-v` when the write was skipped (a newer file) or
/// failed. A corrupt file is replaced.
pub fn write(paths: &Paths, update: ReleaseMemo, max: usize) -> Option<String> {
    let path = paths.release_memo();
    let mut memo = match store::load::<ReleaseMemo>(&path) {
        Ok(Some(loaded)) if loaded.status == VersionStatus::Newer => {
            return Some(format!(
                "release memo: {} was written by a newer omaddons; not updated",
                path.display()
            ));
        }
        Ok(Some(loaded)) => loaded.value,
        Ok(None) | Err(_) => ReleaseMemo::default(),
    };
    memo.merge(update);
    memo.cap(max);
    let written = (|| -> Result<()> {
        let json = store::encode(&memo)?;
        let scopes = ScopeTable::new(paths, &[], None)?;
        let mut s = WriteSession::untracked(Writer::Derived, &scopes, "release-memo");
        ensure_dir_all(&mut s, paths.cache_dir())?;
        let op = Op::WriteFile {
            content: Content::Bytes(&json),
            mode: Some(0o644),
        };
        mutate(&mut s, &path, op)?;
        Ok(())
    })();
    written
        .err()
        .map(|error| format!("release memo not written: {}", error.message))
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
        };
        let fact = |verdicts| CandidateFact {
            release_id: "gh:o/foo@1".into(),
            asset: "Foo.zip".into(),
            sha256: "aa".into(),
            size: 1,
            verdicts,
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

    #[test]
    fn facts_merge_per_asset_and_the_index_finds_hashes_and_verdicts() {
        let verdict = |rules: &str, interface, loads, when| VerdictFact {
            rules: rules.into(),
            interface,
            loads,
            out_of_date: false,
            primary_interface: Some(interface),
            at: at(when),
        };
        let fact = |asset: &str, sha: &str, verdicts| CandidateFact {
            release_id: "gh:o/foo@1".into(),
            asset: asset.into(),
            sha256: sha.into(),
            size: 7,
            verdicts,
        };
        let mut facts = Vec::new();
        merge_fact(
            &mut facts,
            fact(
                "Foo.zip",
                "aa",
                vec![verdict("mainline", 120_105, true, "2026-10-15T10:00:00Z")],
            ),
        );
        merge_fact(
            &mut facts,
            fact(
                "Foo.zip",
                "bb",
                vec![
                    verdict("mainline", 120_105, false, "2026-10-15T11:00:00Z"),
                    verdict("mainline", 120_200, true, "2026-10-15T11:00:00Z"),
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
        let found = index.verdict("gh:o/foo@1", "Foo.zip", "mainline", 120_105);
        assert_eq!(found.map(|v| v.loads), Some(false), "the later verdict");
        assert!(
            index
                .verdict("gh:o/foo@1", "Foo.zip", "mainline", 120_000)
                .is_none(),
            "another interface is another verdict"
        );
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
                verdicts: vec![VerdictFact {
                    rules: "mainline".into(),
                    interface: 1,
                    loads: true,
                    out_of_date: false,
                    primary_interface: None,
                    at: at(when),
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

    #[test]
    fn reads_are_tolerant_and_writes_merge_with_the_file() {
        let (_dir, paths) = paths();
        assert_eq!(load(&paths), (ReleaseMemo::default(), None), "missing");

        let mut first = ReleaseMemo::default();
        first
            .entries
            .insert("a".into(), entry("2026-10-15T12:00:00Z"));
        assert_eq!(write(&paths, first, 10), None);
        let mut second = ReleaseMemo::default();
        second
            .entries
            .insert("b".into(), entry("2026-10-15T12:00:00Z"));
        assert_eq!(write(&paths, second, 10), None);
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
        assert_eq!(write(&paths, third, 10), None, "a corrupt file is replaced");
        assert_eq!(load(&paths).0.entries.keys().collect::<Vec<_>>(), ["c"]);

        let newer = r#"{"schema_version": "9.0", "entries": {}}"#;
        std::fs::write(paths.release_memo(), newer).unwrap();
        let (memo, note) = load(&paths);
        assert!(memo.entries.is_empty() && note.is_some());
        let note = write(&paths, ReleaseMemo::default(), 10);
        assert!(note.is_some_and(|n| n.contains("newer")));
        assert_eq!(
            std::fs::read_to_string(paths.release_memo()).unwrap(),
            newer,
            "a newer memo is never overwritten"
        );
    }
}
