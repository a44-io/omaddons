//! `network`: whether GitHub's REST budget is left, whether GitHub takes the configured
//! token, and whether the sources the installed addons come from answer (M4-D51). These
//! checks run only under `--online` (without it the run lists them `skipped` with
//! `online_required` and never calls them), each through the doctor's probe client
//! ([`crate::doctor::online`]): one request per probe, no retry, no followed redirect, its
//! whole call bounded by the check's budget.
//!
//! A probe that got no answer the check can judge (no client, a status it does not know
//! what to make of, a body it cannot read, or a failure of omaddons' own) is `unanswered`:
//! incomplete coverage, never a clean result and never a false "unreachable". A token, a
//! header or a body never enters evidence (M4-D42).

use crate::doctor::budget::Budget;
use crate::doctor::finding::{Confidence, Detected, Evidence, Observation, SecretOrigin, Subject};
use crate::doctor::online::{Online, PROBED, PURPOSE, Probe, probe_host, project_probe};
use crate::doctor::selection::Selection;
use crate::doctor::view::DoctorView;
use crate::error::ErrorCode;
use crate::model::Ref;
use crate::net::Request;
use crate::politeness::GITHUB_REST_RESERVE;
use crate::sources::SourceId;
use crate::sources::github_rest::{RateLimit, parse_rate_limit, rate_limit_request};

/// Every `evidence.reason` these checks give: the credential's (`rejected`,
/// `unresolvable`) and a source's (`no_answer`, `server_error`, `too_many_requests`).
pub(crate) const REASONS: [&str; 5] = [
    "no_answer",
    "rejected",
    "server_error",
    "too_many_requests",
    "unresolvable",
];

/// `/rate_limit` as a probe, with the token when one resolves (`None` when none is
/// configured); `Err` when a configured one cannot be had.
fn rate_limit_probe(online: &Online) -> Result<(Request, Option<SecretOrigin>), ()> {
    let req = rate_limit_request(PURPOSE).manual_redirects();
    match online.token().value() {
        Err(_) => Err(()),
        Ok(None) => Ok((req, None)),
        Ok(Some(token)) => {
            // ubs:ignore — an outbound request header to api.github.com; github_token::valid rejects controls and spaces
            let req = req.header("Authorization", format!("Bearer {token}"));
            Ok((req, SecretOrigin::of_token(online.token().source())))
        }
    }
}

/// A check whose probe ran out of time: what it found so far, and the deadline that
/// stopped it (recorded now, if the probe saw it pass first).
fn out_of_time(found: Vec<Observation>, budget: &mut Budget) -> Detected {
    let _ = budget.check();
    match budget.incomplete() {
        Some(_) => Detected::cut_short(found, budget),
        // Only a budget with a deadline runs out: never a pass all the same.
        None => Detected::unanswered(found),
    }
}

/// A number header of an answer.
fn header_u64(probe: &Probe, name: &str) -> Option<u64> {
    probe.header(name)?.trim().parse().ok()
}

/// A 403 or 429 saying no request remains: a spent budget, whoever's token it is.
fn spent(status: u16, probe: &Probe) -> bool {
    // ubs:ignore — a count of requests left, not a secret
    matches!(status, 403 | 429) && header_u64(probe, "x-ratelimit-remaining") == Some(0)
}

/// The budget a spent answer's headers report, when they report all of it.
fn rate_limit_from_headers(probe: &Probe) -> Option<RateLimit> {
    let reset = i64::try_from(header_u64(probe, "x-ratelimit-reset")?).ok()?;
    Some(RateLimit {
        limit: header_u64(probe, "x-ratelimit-limit")?,
        remaining: header_u64(probe, "x-ratelimit-remaining")?,
        reset_at: jiff::Timestamp::from_second(reset).ok()?,
    })
}

/// `github-rate-limited`: GitHub's `/rate_limit` (which costs none of the budget) says
/// at most `GITHUB_REST_RESERVE` core requests remain, for the token omaddons sends or,
/// without one, for this address. A token that cannot be had or that GitHub refuses is
/// `credential-rejected`'s: this check is then unanswered.
pub(crate) fn github_rate_limited(
    view: &DoctorView,
    _: &Selection,
    budget: &mut Budget,
) -> Detected {
    let Ok(online) = view.online() else {
        return Detected::unanswered(Vec::new());
    };
    let Ok((req, origin)) = rate_limit_probe(online) else {
        return Detected::unanswered(Vec::new());
    };
    let probe = online.send(req, budget);
    let rate_limit = match &probe {
        Probe::OutOfTime => return out_of_time(Vec::new(), budget),
        Probe::Failed(_) => return Detected::unanswered(Vec::new()),
        Probe::Answered {
            status: 200, body, ..
        } => match parse_rate_limit(body) {
            Some(r) if r.remaining <= u64::from(GITHUB_REST_RESERVE) => Some(r),
            Some(_) => return Detected::complete(Vec::new()),
            None => return Detected::unanswered(Vec::new()),
        },
        Probe::Answered { status, .. } if spent(*status, &probe) => rate_limit_from_headers(&probe),
        // 401 (credential-rejected's finding) and anything else: nothing to judge.
        Probe::Answered { .. } => return Detected::unanswered(Vec::new()),
    };
    Detected::complete(vec![Observation {
        subject: Subject::machine(),
        title: "GitHub's REST API budget is spent".to_owned(),
        confidence: Confidence::High,
        addon: None,
        folder: None,
        evidence: Evidence {
            rate_limit,
            authenticated: Some(origin.is_some()),
            origin,
            ..Evidence::default()
        },
    }])
}

/// `credential-rejected`: GitHub answers 401 to the configured token, or the configured
/// credential cannot be had at all (a missing variable, an unreadable file, a failing
/// command, a value a header cannot carry), which fails every REST request the same way.
/// Nothing is configured: nothing to check, and no request.
pub(crate) fn credential_rejected(
    view: &DoctorView,
    _: &Selection,
    budget: &mut Budget,
) -> Detected {
    let Ok(online) = view.online() else {
        return Detected::unanswered(Vec::new());
    };
    let token = online.token();
    let Some(origin) = SecretOrigin::of_token(token.source()) else {
        return Detected::complete(Vec::new());
    };
    let finding = |reason: &'static str, http_status: Option<u16>| Observation {
        subject: Subject::machine(),
        title: match reason {
            "rejected" => "GitHub rejects the configured token",
            _ => "The configured GitHub credential cannot be read",
        }
        .to_owned(),
        confidence: Confidence::High,
        addon: None,
        folder: None,
        evidence: Evidence {
            reason: Some(reason),
            http_status,
            origin: Some(origin),
            source: Some(SourceId::Github),
            ..Evidence::default()
        },
    };
    let req = match rate_limit_probe(online) {
        Err(()) => return Detected::complete(vec![finding("unresolvable", None)]),
        // A configured source that resolved to no token: nothing is sent, nothing refused.
        Ok((_, None)) => return Detected::complete(Vec::new()),
        Ok((req, Some(_))) => req,
    };
    let probe = online.send(req, budget);
    match &probe {
        Probe::OutOfTime => out_of_time(Vec::new(), budget),
        Probe::Answered { status: 401, .. } => {
            Detected::complete(vec![finding("rejected", Some(401))])
        }
        Probe::Answered { status, .. } if (200..300).contains(status) || spent(*status, &probe) => {
            Detected::complete(Vec::new())
        }
        Probe::Answered { .. } | Probe::Failed(_) => Detected::unanswered(Vec::new()),
    }
}

/// The one project each probed source is asked about: the first addon of that source on
/// any target whose record was read (whatever `-t` says), in (target id, addon key)
/// order.
fn projects(view: &DoctorView) -> Vec<(SourceId, Ref)> {
    let Some(records) = view.records() else {
        return Vec::new();
    };
    let mut addons: Vec<(String, &str, &Ref)> = records
        .available()
        .flat_map(|(key, loaded)| {
            let id = view.target_ref(*key).id;
            loaded
                .value
                .addons
                .iter()
                .map(move |(addon_key, addon)| (id.clone(), addon_key.as_str(), &addon.r#ref))
        })
        .collect();
    addons.sort_by(|a, b| (&a.0, a.1).cmp(&(&b.0, b.1)));
    PROBED
        .into_iter()
        .filter_map(|source| {
            addons
                .iter()
                // ubs:ignore — two source ids, not a secret
                .find(|(_, _, r)| SourceId::of(r) == source)
                .map(|(_, _, r)| (source, (*r).clone()))
        })
        .collect()
}

/// `source-unreachable`: one request to each enabled source an installed addon comes
/// from (GitHub, WoWInterface, Tukui, WowAce, in that order), for its first project. An
/// answer below 500 other than a 429 is an answer, a 404 included (a project that is
/// gone is not this failure mode); a 5xx or a 429 is a finding; no answer at all is one
/// of medium confidence, since the request may have failed on this machine's own network.
/// A probe that failed for omaddons' own reasons leaves that source unanswered, and the
/// others are still asked.
pub(crate) fn source_unreachable(
    view: &DoctorView,
    _: &Selection,
    budget: &mut Budget,
) -> Detected {
    let Ok(online) = view.online() else {
        return Detected::unanswered(Vec::new());
    };
    let mut found = Vec::new();
    let mut unanswered = false;
    for (source, project) in projects(view) {
        if !online.enabled(source) {
            continue;
        }
        let Some(req) = project_probe(&project) else {
            continue;
        };
        if budget.check().is_err() {
            return Detected::cut_short(found, budget);
        }
        let host = probe_host(source);
        let (reason, http_status, confidence) = match online.send(req, budget) {
            Probe::OutOfTime => return out_of_time(found, budget),
            Probe::Answered { status: 429, .. } => {
                ("too_many_requests", Some(429), Confidence::High)
            }
            Probe::Answered { status, .. } if status >= 500 => {
                ("server_error", Some(status), Confidence::High)
            }
            Probe::Answered { .. } => continue,
            // ubs:ignore — an error code, not a secret
            Probe::Failed(error) if error.code == ErrorCode::Network => {
                ("no_answer", None, Confidence::Medium)
            }
            Probe::Failed(_) => {
                unanswered = true;
                continue;
            }
        };
        found.push(Observation {
            subject: Subject::machine().source(source),
            title: format!("{} does not answer", source.name()),
            confidence,
            addon: None,
            folder: None,
            evidence: Evidence {
                reason: Some(reason),
                source: Some(source),
                host: Some(host.to_owned()),
                http_status,
                ..Evidence::default()
            },
        });
    }
    if unanswered {
        Detected::unanswered(found)
    } else {
        Detected::complete(found)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use super::*;
    use crate::doctor::finding::{Finding, Incomplete, IncompleteLimit, InstanceKey};
    use crate::doctor::online::OnlineUnavailable;
    use crate::doctor::online::fake::{Answer, Sent, online};
    use crate::doctor::registry::{self, ids};
    use crate::doctor::view::tests::{K, K2, Plant, home, key, plant, registry_bytes};
    use crate::doctor::view::{LayerState, Preloaded, Records};
    use crate::model::{
        AddonReason, AddonRecord, AddonState, Channel, FolderName, VersionConfidence,
    };
    use crate::net::{Method, RedirectMode};
    use crate::sources::github_token::{GithubToken, TokenSource};
    use crate::state::config::LoadedConfig;
    use crate::state::records::TargetRecord;
    use crate::state::store::{Loaded, VersionStatus, Versioned};

    const HOUR: Duration = Duration::from_secs(3600);
    const RATE: &str = "api.github.com/rate_limit";
    /// The host `/rate_limit` is asked on: the only host a token is ever sent to.
    const API_HOST: &str = "api.github.com";

    /// Every URL the three rows can build, one per source and `/rate_limit`.
    pub(crate) fn probe_urls() -> Vec<String> {
        let mut urls = vec![rate_limit_request(PURPOSE).url];
        for r in [
            "gh:o/r",
            "wowi:5108",
            "tukui:elvui",
            "wowace:bug-grabber",
            "wowace:bug-grabber#r152",
        ] {
            urls.push(project_probe(&r.parse().unwrap()).unwrap().url);
        }
        urls
    }

    fn addon(name: &str, r: &str) -> AddonRecord {
        AddonRecord {
            r#ref: r.parse().unwrap(),
            name: name.into(),
            version: "1.0".into(),
            version_confidence: VersionConfidence::Release,
            release_id: None,
            channel: Channel::Stable,
            pinned: false,
            reason: AddonReason::Explicit,
            required_by: vec![],
            folders: vec![FolderName::new(name).unwrap()],
            state: AddonState::Ok,
            installed_at: "2026-10-14T21:00:00Z".parse().unwrap(),
            tx_id: "01JB8Q2VA00000000000000000".parse().unwrap(),
            blob_sha256: None,
            revision: None,
            left_out: Vec::new(),
            any_flavor: false,
        }
    }

    /// Records of the targets `K` and `K2`, each holding `(name, ref)` addons.
    fn records(by_target: &[(&str, &[(&str, &str)])]) -> Records {
        let by_key: BTreeMap<_, _> = by_target
            .iter()
            .map(|(k, addons)| {
                let record = TargetRecord {
                    generation: 1,
                    addons: addons
                        .iter()
                        .map(|(name, r)| (TargetRecord::addon_key(name), addon(name, r)))
                        .collect(),
                    ..TargetRecord::empty(key(k))
                };
                (
                    key(k),
                    LayerState::Available(Loaded {
                        value: record,
                        version: TargetRecord::CURRENT,
                        status: VersionStatus::Current,
                    }),
                )
            })
            .collect();
        Records::from_states(by_key)
    }

    /// A view holding `client` and `records`, its registry naming `K` `retail` and `K2`
    /// `era`.
    struct Scene {
        _dir: tempfile::TempDir,
        view: DoctorView,
        sent: Arc<Mutex<Vec<Sent>>>,
    }

    fn scene(
        table: Vec<(&'static str, Answer)>,
        token: Arc<GithubToken>,
        disabled: &[SourceId],
        records_: Records,
    ) -> Scene {
        let (dir, paths) = home();
        plant(
            &paths.registry_file(),
            &Plant::Bytes(registry_bytes(&[(K, "retail"), (K2, "era")])),
        );
        let (client, sent) = online(table, token, disabled);
        let view = DoctorView::preloaded(
            paths,
            Preloaded {
                config: Some(LayerState::Available(LoadedConfig::default())),
                records: Some(LayerState::Available(records_)),
                online: Some(Ok(client)),
                ..Preloaded::default()
            },
        );
        Scene {
            _dir: dir,
            view,
            sent,
        }
    }

    impl Scene {
        fn sent(&self) -> Vec<Sent> {
            self.sent.lock().unwrap().clone()
        }

        fn run(&self, detect: registry::DetectFn, budget: &mut Budget) -> Detected {
            detect(&self.view, &Selection::all(), budget)
        }
    }

    fn token(source: TokenSource) -> Arc<GithubToken> {
        GithubToken::fixed(source, Some("ghp_secret".to_owned()))
    }

    fn json(body: &str) -> Answer {
        Answer::Status(200, Vec::new(), body.as_bytes().to_vec())
    }

    fn budget_body(remaining: u64) -> Answer {
        json(&format!(
            r#"{{"resources":{{"core":{{"limit":60,"remaining":{remaining},"reset":1793600000}}}}}}"#
        ))
    }

    fn spent_answer(status: u16) -> Answer {
        Answer::Status(
            status,
            vec![
                ("x-ratelimit-limit", "5000".to_owned()),
                ("x-ratelimit-remaining", "0".to_owned()),
                ("x-ratelimit-reset", "1793600000".to_owned()),
            ],
            Vec::new(),
        )
    }

    fn unanswered() -> Option<Incomplete> {
        Some(Incomplete {
            limit: IncompleteLimit::Unanswered,
            deadline_ms: None,
            max_entries: None,
        })
    }

    /// `id`'s findings for `detected`, as the registry row makes them.
    fn findings(id: &str, detected: &Detected) -> Vec<Finding> {
        let row = registry::row(id).unwrap();
        detected
            .found
            .iter()
            .cloned()
            .map(|obs| row.finding(obs))
            .collect()
    }

    fn reset() -> jiff::Timestamp {
        jiff::Timestamp::from_second(1_793_600_000).unwrap()
    }

    #[test]
    fn rate_limited_cases() {
        struct Case {
            name: &'static str,
            answer: Option<Answer>,
            token: Arc<GithubToken>,
            /// The finding's `rate_limit`, `authenticated` and `origin`, or none.
            expected: Option<(Option<RateLimit>, bool, Option<SecretOrigin>)>,
            incomplete: Option<Incomplete>,
            requests: usize,
        }
        let unresolvable = GithubToken::command(vec!["/nonexistent/omaddons-credential".into()]);
        let limit = |limit, remaining| {
            Some(RateLimit {
                limit,
                remaining,
                reset_at: reset(),
            })
        };
        let cases = [
            Case {
                name: "remaining_59_is_none",
                answer: Some(budget_body(59)),
                token: GithubToken::none(),
                expected: None,
                incomplete: None,
                requests: 1,
            },
            Case {
                name: "remaining_5_is_a_finding",
                answer: Some(budget_body(5)),
                token: GithubToken::none(),
                expected: Some((limit(60, 5), false, None)),
                incomplete: None,
                requests: 1,
            },
            Case {
                name: "a_403_with_none_left",
                answer: Some(spent_answer(403)),
                token: token(TokenSource::File),
                expected: Some((limit(5000, 0), true, Some(SecretOrigin::File))),
                incomplete: None,
                requests: 1,
            },
            Case {
                name: "a_429_with_none_left",
                answer: Some(spent_answer(429)),
                token: token(TokenSource::Env),
                expected: Some((limit(5000, 0), true, Some(SecretOrigin::Env))),
                incomplete: None,
                requests: 1,
            },
            Case {
                name: "a_403_without_the_header_is_unanswered",
                answer: Some(Answer::status(403)),
                token: GithubToken::none(),
                expected: None,
                incomplete: unanswered(),
                requests: 1,
            },
            Case {
                name: "a_401_with_a_token_is_unanswered",
                answer: Some(Answer::status(401)),
                token: token(TokenSource::Config),
                expected: None,
                incomplete: unanswered(),
                requests: 1,
            },
            Case {
                name: "a_500_is_unanswered",
                answer: Some(Answer::status(500)),
                token: GithubToken::none(),
                expected: None,
                incomplete: unanswered(),
                requests: 1,
            },
            Case {
                name: "a_body_that_is_not_json",
                answer: Some(json("not json")),
                token: GithubToken::none(),
                expected: None,
                incomplete: unanswered(),
                requests: 1,
            },
            Case {
                name: "an_unresolvable_token_sends_nothing",
                answer: Some(budget_body(1)),
                token: unresolvable,
                expected: None,
                incomplete: unanswered(),
                requests: 0,
            },
            Case {
                name: "a_network_error_is_unanswered",
                answer: Some(Answer::Fails(ErrorCode::Network)),
                token: GithubToken::none(),
                expected: None,
                incomplete: unanswered(),
                requests: 1,
            },
        ];
        for case in cases {
            let table = case.answer.into_iter().map(|a| (RATE, a)).collect();
            let s = scene(table, case.token, &[], Records::default());
            let detected = s.run(github_rate_limited, &mut Budget::new(HOUR, 10));
            let sent = s.sent();
            let got = detected.found.first().map(|obs| {
                (
                    obs.evidence.rate_limit,
                    obs.evidence.authenticated.unwrap_or_default(),
                    obs.evidence.origin,
                )
            });
            assert_eq!(
                (got, detected.incomplete, sent.len()),
                (case.expected, case.incomplete, case.requests),
                "{}: sent {sent:?}, detected {detected:?}",
                case.name
            );
            assert!(detected.found.len() <= 1, "{}: {detected:?}", case.name);
            for request in &sent {
                assert_eq!(
                    (request.purpose, request.redirect, request.host()),
                    (PURPOSE, RedirectMode::Manual, API_HOST),
                    "{}",
                    case.name
                );
            }
        }
    }

    #[test]
    fn rate_limited_remediation_texts() {
        for (name, token, expected) in [
            (
                "without_a_token",
                GithubToken::none(),
                "wait until 2026-11-02 06:13 UTC, or pipe a token to omaddons auth github set for 5,000 requests an hour",
            ),
            (
                "with_a_token",
                token(TokenSource::File),
                "wait until 2026-11-02 06:13 UTC",
            ),
        ] {
            let s = scene(vec![(RATE, budget_body(0))], token, &[], Records::default());
            let detected = s.run(github_rate_limited, &mut Budget::new(HOUR, 10));
            let found = findings(ids::NETWORK_GITHUB_RATE_LIMITED, &detected);
            assert_eq!(found.len(), 1, "{name}: {detected:?}");
            let f = &found[0];
            assert_eq!(
                (f.remediation.command.as_str(), &f.remediation.argv),
                (expected, &None),
                "{name}"
            );
            assert_eq!(
                (f.instance.as_str(), f.label.as_str()),
                ("fm-network-github-rate-limited@machine", "machine"),
                "{name}"
            );
        }
    }

    #[test]
    fn an_expired_budget_cuts_each_row_short_and_sends_nothing() {
        let detectors: [(&str, registry::DetectFn); 3] = [
            ("rate_limited", github_rate_limited),
            ("credential_rejected", credential_rejected),
            ("source_unreachable", source_unreachable),
        ];
        for (name, detect) in detectors {
            let s = scene(
                vec![(RATE, budget_body(0)), ("github.com", Answer::status(503))],
                token(TokenSource::File),
                &[],
                records(&[(K, &[("Foo", "gh:o/foo")])]),
            );
            let mut budget = Budget::new(Duration::from_millis(1), 10);
            std::thread::sleep(Duration::from_millis(5));
            let detected = s.run(detect, &mut budget);
            assert_eq!(
                (
                    detected.found.len(),
                    detected.incomplete.map(|i| i.limit),
                    s.sent().len()
                ),
                (0, Some(IncompleteLimit::Deadline), 0),
                "{name}: {detected:?}"
            );
        }
    }

    #[test]
    fn credential_rejected_cases() {
        struct Case {
            name: &'static str,
            token: Arc<GithubToken>,
            answer: Answer,
            /// The finding's reason, status and origin, or none.
            expected: Option<(&'static str, Option<u16>, SecretOrigin)>,
            incomplete: Option<Incomplete>,
            requests: usize,
        }
        let cases = [
            Case {
                name: "no_credential_sends_nothing",
                token: GithubToken::none(),
                answer: Answer::status(401),
                expected: None,
                incomplete: None,
                requests: 0,
            },
            Case {
                name: "env_answered_401",
                token: token(TokenSource::Env),
                answer: Answer::status(401),
                expected: Some(("rejected", Some(401), SecretOrigin::Env)),
                incomplete: None,
                requests: 1,
            },
            Case {
                name: "config_answered_401",
                token: token(TokenSource::Config),
                answer: Answer::status(401),
                expected: Some(("rejected", Some(401), SecretOrigin::Config)),
                incomplete: None,
                requests: 1,
            },
            Case {
                name: "file_answered_401",
                token: token(TokenSource::File),
                answer: Answer::status(401),
                expected: Some(("rejected", Some(401), SecretOrigin::File)),
                incomplete: None,
                requests: 1,
            },
            Case {
                name: "a_200_is_none",
                token: token(TokenSource::File),
                answer: budget_body(4000),
                expected: None,
                incomplete: None,
                requests: 1,
            },
            Case {
                name: "a_403_with_a_spent_budget_is_none",
                token: token(TokenSource::File),
                answer: spent_answer(403),
                expected: None,
                incomplete: None,
                requests: 1,
            },
            Case {
                name: "an_unresolvable_command_sends_nothing",
                token: GithubToken::command(vec!["/nonexistent/omaddons-credential".into()]),
                answer: Answer::status(200),
                expected: Some(("unresolvable", None, SecretOrigin::Config)),
                incomplete: None,
                requests: 0,
            },
            Case {
                name: "a_network_error_is_unanswered",
                token: token(TokenSource::Env),
                answer: Answer::Fails(ErrorCode::Network),
                expected: None,
                incomplete: unanswered(),
                requests: 1,
            },
            Case {
                name: "a_500_is_unanswered",
                token: token(TokenSource::Env),
                answer: Answer::status(500),
                expected: None,
                incomplete: unanswered(),
                requests: 1,
            },
        ];
        for case in cases {
            let s = scene(
                vec![(RATE, case.answer)],
                case.token,
                &[],
                Records::default(),
            );
            let detected = s.run(credential_rejected, &mut Budget::new(HOUR, 10));
            let sent = s.sent();
            let got = detected.found.first().map(|obs| {
                (
                    obs.evidence.reason.unwrap_or_default(),
                    obs.evidence.http_status,
                    obs.evidence.origin.unwrap(),
                )
            });
            assert_eq!(
                (got, detected.incomplete, sent.len()),
                (case.expected, case.incomplete, case.requests),
                "{}: sent {sent:?}, detected {detected:?}",
                case.name
            );
            for request in &sent {
                assert!(request.authorized(), "{}: {request:?}", case.name);
                assert_eq!(request.host(), API_HOST, "{}", case.name);
            }
            for obs in &detected.found {
                assert_eq!(obs.evidence.source, Some(SourceId::Github), "{}", case.name);
            }
        }
    }

    /// A missing variable a config `env` credential names: the configured credential
    /// cannot be had, so nothing is sent.
    #[test]
    fn a_config_env_credential_whose_variable_is_unset_is_unresolvable() {
        let (_dir, paths) = home();
        let var = "OMADDONS_TEST_CREDENTIAL_THAT_IS_NEVER_SET";
        assert!(std::env::var_os(var).is_none());
        let sources = BTreeMap::from([(
            "github".to_owned(),
            crate::state::config::SourceConfig {
                credential: Some(crate::state::config::CredentialRef::Env { env: var.into() }),
                ..Default::default()
            },
        )]);
        let token = GithubToken::resolve_with(&|_| None, &paths, &sources);
        let s = scene(
            vec![(RATE, Answer::status(200))],
            token,
            &[],
            Records::default(),
        );
        let detected = s.run(credential_rejected, &mut Budget::new(HOUR, 10));
        assert_eq!(s.sent().len(), 0);
        assert_eq!(
            detected.found.first().and_then(|obs| obs.evidence.reason),
            Some("unresolvable"),
            "{detected:?}"
        );
    }

    #[test]
    fn credential_rejected_remediation_by_origin() {
        let cases = [
            (
                TokenSource::File,
                "omaddons auth github clear",
                Some(vec!["omaddons", "auth", "github", "clear"]),
            ),
            (
                TokenSource::Env,
                "replace or unset OMADDONS_GITHUB_TOKEN",
                None,
            ),
            (
                TokenSource::Config,
                "correct [sources.github] credential in config.toml",
                None,
            ),
        ];
        for (source, command, argv) in cases {
            let s = scene(
                vec![(RATE, Answer::status(401))],
                token(source),
                &[],
                Records::default(),
            );
            let detected = s.run(credential_rejected, &mut Budget::new(HOUR, 10));
            let found = findings(ids::NETWORK_CREDENTIAL_REJECTED, &detected);
            assert_eq!(found.len(), 1, "{source:?}: {detected:?}");
            let r = &found[0].remediation;
            let argv: Option<Vec<String>> =
                argv.map(|argv| argv.into_iter().map(str::to_owned).collect());
            assert_eq!(
                (r.command.as_str(), &r.argv, r.destructive),
                (command, &argv, false),
                "{source:?}"
            );
            assert_eq!(found[0].title, "GitHub rejects the configured token");
        }
    }

    #[test]
    fn source_unreachable_cases() {
        let all: &[(&str, &[(&str, &str)])] = &[
            (
                K,
                &[
                    ("Bar", "wowi:5108"),
                    ("Elv", "tukui:elvui"),
                    ("Foo", "gh:o/foo"),
                    ("Grab", "wowace:bug-grabber#r152"),
                    ("Loc", "file:/tmp/Loc.zip"),
                    ("Git", "git:https://codeberg.org/o/r#main"),
                    (
                        "Url",
                        "url:https://github.com/o/r/releases/download/v1/x.zip",
                    ),
                ],
            ),
            (K2, &[("Aaa", "gh:first/aaa")]),
        ];
        struct Case {
            name: &'static str,
            records: Records,
            table: Vec<(&'static str, Answer)>,
            disabled: &'static [SourceId],
            /// The requests sent, in order, as `(method, url)`.
            sent: Vec<(Method, &'static str)>,
            /// Each finding's source, reason, status and confidence.
            found: Vec<(SourceId, &'static str, Option<u16>, Confidence)>,
            incomplete: Option<Incomplete>,
        }
        let gh_era = "https://github.com/first/aaa/releases/latest";
        let wowi = "https://api.mmoui.com/v3/game/WOW/filedetails/5108.json";
        let tukui = "https://api.tukui.org/v1/addons";
        let wowace = "https://repos.wowace.com/wow/bug-grabber/tags/";
        let four = vec![
            (Method::Get, gh_era),
            (Method::Get, wowi),
            (Method::Get, tukui),
            (Method::Propfind, wowace),
        ];
        let cases = vec![
            Case {
                name: "no_http_source_sends_nothing",
                records: records(&[(K, &[("Loc", "file:/tmp/Loc.zip")])]),
                table: vec![],
                disabled: &[],
                sent: vec![],
                found: vec![],
                incomplete: None,
            },
            Case {
                name: "all_four_in_order_for_the_first_project_by_target_id",
                records: records(all),
                table: vec![
                    ("github.com", Answer::status(302)),
                    ("mmoui", Answer::status(200)),
                    ("tukui", Answer::status(404)),
                    ("wowace", Answer::status(207)),
                ],
                disabled: &[],
                sent: four.clone(),
                found: vec![],
                incomplete: None,
            },
            Case {
                name: "a_disabled_source_is_not_probed",
                records: records(all),
                table: vec![("", Answer::status(200))],
                disabled: &[SourceId::Wowi, SourceId::Tukui],
                sent: vec![(Method::Get, gh_era), (Method::Propfind, wowace)],
                found: vec![],
                incomplete: None,
            },
            Case {
                name: "server_errors_and_429_and_no_answer",
                records: records(all),
                table: vec![
                    ("github.com", Answer::status(503)),
                    ("mmoui", Answer::status(429)),
                    ("tukui", Answer::Fails(ErrorCode::Network)),
                    ("wowace", Answer::status(500)),
                ],
                disabled: &[],
                sent: four.clone(),
                found: vec![
                    (
                        SourceId::Github,
                        "server_error",
                        Some(503),
                        Confidence::High,
                    ),
                    (
                        SourceId::Wowi,
                        "too_many_requests",
                        Some(429),
                        Confidence::High,
                    ),
                    (SourceId::Tukui, "no_answer", None, Confidence::Medium),
                    (
                        SourceId::Wowace,
                        "server_error",
                        Some(500),
                        Confidence::High,
                    ),
                ],
                incomplete: None,
            },
            Case {
                name: "an_internal_failure_leaves_its_source_unanswered",
                records: records(all),
                table: vec![
                    ("mmoui", Answer::Fails(ErrorCode::Internal)),
                    ("tukui", Answer::status(503)),
                    ("", Answer::status(200)),
                ],
                disabled: &[],
                sent: four.clone(),
                found: vec![(SourceId::Tukui, "server_error", Some(503), Confidence::High)],
                incomplete: unanswered(),
            },
            Case {
                name: "git_url_and_file_records_are_never_probed",
                records: records(&[(
                    K,
                    &[
                        ("Loc", "file:/tmp/Loc.zip"),
                        ("Git", "git:https://codeberg.org/o/r#main"),
                        (
                            "Url",
                            "url:https://github.com/o/r/releases/download/v1/x.zip",
                        ),
                    ],
                )]),
                table: vec![],
                disabled: &[],
                sent: vec![],
                found: vec![],
                incomplete: None,
            },
        ];
        for case in cases {
            let s = scene(case.table, GithubToken::none(), case.disabled, case.records);
            let detected = s.run(source_unreachable, &mut Budget::new(HOUR, 10));
            let sent: Vec<(Method, String)> =
                s.sent().into_iter().map(|r| (r.method, r.url)).collect();
            let expected_sent: Vec<(Method, String)> = case
                .sent
                .iter()
                .map(|(m, u)| (*m, (*u).to_owned()))
                .collect();
            let found: Vec<_> = detected
                .found
                .iter()
                .map(|obs| {
                    (
                        obs.evidence.source.unwrap(),
                        obs.evidence.reason.unwrap(),
                        obs.evidence.http_status,
                        obs.confidence,
                    )
                })
                .collect();
            assert_eq!(
                (sent, found, detected.incomplete),
                (expected_sent, case.found, case.incomplete),
                "{}: {detected:?}",
                case.name
            );
            for request in s.sent() {
                assert_eq!(
                    (request.purpose, request.redirect, request.authorized()),
                    (PURPOSE, RedirectMode::Manual, false),
                    "{}: {request:?}",
                    case.name
                );
            }
            for obs in &detected.found {
                let source = obs.evidence.source.unwrap();
                assert_eq!(
                    obs.evidence.host.as_deref(),
                    Some(probe_host(source)),
                    "{}",
                    case.name
                );
                assert_eq!(obs.title, format!("{} does not answer", source.name()));
            }
        }
    }

    /// GitHub's 503 comes back at once; WoWInterface's probe outlives the deadline, so
    /// the check stops there, keeps GitHub's finding and never asks Tukui.
    #[test]
    fn a_budget_that_expires_after_the_first_probe_keeps_its_finding() {
        let stalled = scene(
            vec![
                ("github.com", Answer::status(503)),
                ("mmoui", Answer::Stalls(Duration::from_millis(60))),
            ],
            GithubToken::none(),
            &[],
            records(&[(
                K,
                &[
                    ("Foo", "gh:o/foo"),
                    ("Bar", "wowi:5108"),
                    ("Elv", "tukui:elvui"),
                ],
            )]),
        );
        let mut budget = Budget::new(Duration::from_millis(30), 10);
        let detected = stalled.run(source_unreachable, &mut budget);
        let sources: Vec<SourceId> = detected
            .found
            .iter()
            .filter_map(|obs| obs.evidence.source)
            .collect();
        assert_eq!(sources, [SourceId::Github], "{detected:?}");
        assert_eq!(
            detected.incomplete.map(|i| i.limit),
            Some(IncompleteLimit::Deadline),
            "{detected:?}"
        );
        assert_eq!(
            stalled.sent().len(),
            2,
            "Tukui was never asked: {:?}",
            stalled.sent()
        );
    }

    #[test]
    fn source_discriminator_round_trips() {
        for source in PROBED {
            let s = scene(
                vec![("", Answer::status(503))],
                GithubToken::none(),
                &[],
                records(&[(
                    K,
                    &[(
                        "Foo",
                        match source {
                            SourceId::Github => "gh:o/foo",
                            SourceId::Wowi => "wowi:1",
                            SourceId::Tukui => "tukui:elvui",
                            _ => "wowace:x",
                        },
                    )],
                )]),
            );
            let detected = s.run(source_unreachable, &mut Budget::new(HOUR, 10));
            let found = findings(ids::NETWORK_SOURCE_UNREACHABLE, &detected);
            assert_eq!(found.len(), 1, "{source:?}: {detected:?}");
            let instance = format!(
                "fm-network-source-unreachable@machine:source={}",
                source.wire()
            );
            assert_eq!(found[0].instance.as_str(), instance);
            assert_eq!(found[0].label, format!("machine · {}", source.name()));
            let parsed = InstanceKey::parse(&instance).unwrap();
            assert_eq!(parsed.to_string(), instance);
            assert_eq!(
                found[0].remediation.command,
                format!(
                    "check this machine's network connection and {}, then run the online checks again later",
                    probe_host(source)
                )
            );
            assert_eq!(found[0].remediation.argv, None);
        }
        assert!(InstanceKey::parse("fm-network-source-unreachable@machine:source=nope").is_err());
    }

    /// One view, one client, every row: the token goes only to `api.github.com`.
    #[test]
    fn the_token_goes_only_to_the_api_host() {
        let s = scene(
            vec![(RATE, budget_body(30)), ("", Answer::status(200))],
            token(TokenSource::Env),
            &[],
            records(&[(
                K,
                &[
                    ("Foo", "gh:o/foo"),
                    ("Bar", "wowi:5108"),
                    ("Elv", "tukui:elvui"),
                    ("Grab", "wowace:bug-grabber"),
                ],
            )]),
        );
        for detect in [github_rate_limited, credential_rejected, source_unreachable] {
            let _ = s.run(detect, &mut Budget::new(HOUR, 10));
        }
        let sent = s.sent();
        assert_eq!(sent.len(), 6, "{sent:?}");
        for request in &sent {
            assert_eq!(
                request.authorized(),
                request.host() == API_HOST,
                "{request:?}"
            );
        }
    }

    /// Without a client (not asked for `--online`, or `net.log` unwritable), every row is
    /// unanswered and sends nothing.
    #[test]
    fn no_client_is_unanswered() {
        let (_dir, paths) = home();
        for why in [
            OnlineUnavailable::Disabled,
            OnlineUnavailable::NoConfig,
            OnlineUnavailable::NetLog(crate::error::Error::internal("no log")),
        ] {
            let view = DoctorView::preloaded(
                paths.clone(),
                Preloaded {
                    online: Some(Err(why.clone())),
                    ..Preloaded::default()
                },
            );
            for detect in [github_rate_limited, credential_rejected, source_unreachable] {
                let detected = detect(&view, &Selection::all(), &mut Budget::new(HOUR, 10));
                assert_eq!(
                    (detected.found.len(), detected.incomplete),
                    (0, unanswered()),
                    "{why:?}"
                );
            }
        }
    }
}
