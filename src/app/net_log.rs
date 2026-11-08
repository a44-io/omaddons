//! `omaddons net-log [--since D]`: every HTTP request and git subprocess omaddons made
//! (vision G5, D13), from `net.log` and its one rotated predecessor, oldest first, with
//! counts per host and per transport. Secrets were stripped when each line was written;
//! this command only parses. Read-only and never on the network.

use std::collections::BTreeMap;
use std::io::Write;

use jiff::Timestamp;
use schemars::JsonSchema;
use serde::Serialize;

use super::history::parse_since;
use crate::cli::Context;
use crate::error::{Error, ErrorCode, Result, Suggestion, Warning};
use crate::net::NetLogEntry;
use crate::net::log::read_log;
use crate::output::{Outcome, Style, table};
use crate::state::paths::Paths;

/// The default `--limit`: token-frugal, yet a whole run of a few clients.
const DEFAULT_LIMIT: usize = 50;

/// `net-log`'s own flags.
#[derive(Debug, Clone, Default, clap::Args)]
pub struct NetLogArgs {
    /// At most this many requests, the most recent (default 50)
    #[arg(long, value_name = "N")]
    pub limit: Option<usize>,
    /// Only requests since this: an age (30m, 12h, 1d, 2w) or a time
    ///
    /// A time is RFC 3339.
    #[arg(long, value_name = "WHEN")]
    pub since: Option<String>,
}

/// `net-log`'s data.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct NetLogData {
    /// The most recent `--limit` requests that matched, oldest first.
    pub entries: Vec<NetLogEntry>,
    /// More requests matched than `entries` shows.
    pub truncated: bool,
    /// Every request that matched, by host and by transport.
    pub summary: Summary,
    /// Lines that do not parse.
    pub skipped: usize,
    /// The instant `--since` resolved to: only requests at or after it are counted and
    /// shown. `null` without `--since`.
    #[schemars(schema_with = "crate::contract::schema::timestamp_opt")]
    pub since_at: Option<Timestamp>,
}

/// Requests per host and per transport (`http`, `git`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Summary {
    pub by_host: BTreeMap<String, usize>,
    pub by_via: BTreeMap<String, usize>,
}

impl Summary {
    pub fn of(entries: &[NetLogEntry]) -> Self {
        let mut summary = Self::default();
        for e in entries {
            *summary.by_host.entry(e.host.clone()).or_default() += 1;
            let via = serde_json::to_value(e.via)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_default();
            *summary.by_via.entry(via).or_default() += 1;
        }
        summary
    }
}

/// `net-log`'s outcome.
#[derive(Debug)]
pub struct NetLogOutcome {
    data: NetLogData,
}

impl Outcome for NetLogOutcome {
    type Data = NetLogData;

    fn command(&self) -> &'static str {
        "net-log"
    }

    fn data(&self) -> &NetLogData {
        &self.data
    }

    fn warnings(&self) -> &[Warning] {
        &[]
    }

    fn render_human(&self, w: &mut dyn Write, _style: &Style) -> std::io::Result<()> {
        let d = &self.data;
        if d.entries.is_empty() {
            writeln!(w, "No requests logged.")?;
        } else {
            let rows: Vec<Vec<String>> = d
                .entries
                .iter()
                .map(|e| {
                    vec![
                        e.time.to_string(),
                        format!(
                            "{} {}",
                            e.method,
                            e.status.map_or_else(|| "-".to_owned(), |s| s.to_string())
                        ),
                        e.host.clone(),
                        e.path.clone(),
                        e.bytes.map_or_else(|| "-".to_owned(), |b| format!("{b} B")),
                        format!("{} ms", e.duration_ms),
                        e.purpose.clone(),
                    ]
                })
                .collect();
            write!(w, "{}", table(&rows))?;
            if d.truncated {
                let total: usize = d.summary.by_host.values().sum();
                let earlier = total.saturating_sub(d.entries.len());
                writeln!(w, "({earlier} earlier requests: --limit N or --since)")?;
            }
        }
        let hosts: Vec<String> = d
            .summary
            .by_host
            .iter()
            .map(|(host, n)| format!("{host} {n}"))
            .collect();
        if !hosts.is_empty() {
            writeln!(w, "hosts: {}", hosts.join(", "))?;
        }
        if d.skipped > 0 {
            writeln!(w, "{} unreadable lines skipped", d.skipped)?;
        }
        Ok(())
    }
}

/// Runs `net-log`.
pub fn run(_ctx: &Context, args: &NetLogArgs) -> Result<NetLogOutcome> {
    let limit = args.limit.unwrap_or(DEFAULT_LIMIT);
    if limit == 0 {
        return Err(Error::new(
            ErrorCode::Usage,
            "--limit must be at least 1",
            Suggestion::command(["omaddons", "net-log", "--limit", "50"]),
        ));
    }
    let since = args
        .since
        .as_deref()
        .map(|s| {
            parse_since(s, Timestamp::now()).ok_or_else(|| {
                Error::new(
                    ErrorCode::Usage,
                    format!(
                        "--since takes a duration (30m, 12h, 1d, 2w) or an RFC 3339 time, not {s:?}"
                    ),
                    Suggestion::command(["omaddons", "net-log", "--since", "1d"]),
                )
            })
        })
        .transpose()?;
    let read = read_log(&Paths::from_env()?, since)?;
    Ok(NetLogOutcome {
        data: data(read.entries, read.skipped, since, limit),
    })
}

/// `net-log`'s data from the matching `entries` (oldest first): the last `limit` of
/// them, and the summary of them all.
fn data(
    mut entries: Vec<NetLogEntry>,
    skipped: usize,
    since: Option<Timestamp>,
    limit: usize,
) -> NetLogData {
    let summary = Summary::of(&entries);
    let truncated = entries.len() > limit;
    entries.drain(..entries.len().saturating_sub(limit));
    NetLogData {
        entries,
        truncated,
        summary,
        skipped,
        since_at: since,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::Via;

    #[test]
    fn summary_counts_hosts_and_transports() {
        let entry = |host: &str, via| NetLogEntry {
            time: "2026-10-14T00:00:00Z".parse().unwrap(),
            method: "GET".into(),
            host: host.into(),
            path: "/".into(),
            status: Some(200),
            bytes: Some(1),
            wire_bytes: None,
            duration_ms: 1,
            purpose: "test".into(),
            via,
            error: None,
        };
        let summary = Summary::of(&[
            entry("github.com", Via::Http),
            entry("github.com", Via::Git),
            entry("codeload.github.com", Via::Http),
        ]);
        assert_eq!(
            serde_json::to_value(&summary).unwrap(),
            serde_json::json!({
                "by_host": {"codeload.github.com": 1, "github.com": 2},
                "by_via": {"git": 1, "http": 2},
            })
        );
    }

    /// The last `limit` requests, oldest first; the summary counts them all.
    #[test]
    fn the_most_recent_requests_are_kept_and_all_are_counted() {
        let entry = |n: u32| NetLogEntry {
            time: Timestamp::from_second(1_760_000_000 + i64::from(n)).unwrap(),
            method: "GET".into(),
            host: "github.com".into(),
            path: format!("/{n}"),
            status: Some(200),
            bytes: Some(1),
            wire_bytes: None,
            duration_ms: 1,
            purpose: "test".into(),
            via: Via::Http,
            error: None,
        };
        let entries: Vec<NetLogEntry> = (0..120).map(entry).collect();
        let d = data(entries.clone(), 0, None, 50);
        assert_eq!(
            (
                d.entries.len(),
                d.truncated,
                d.summary.by_host["github.com"]
            ),
            (50, true, 120)
        );
        assert_eq!(
            (d.entries[0].path.as_str(), d.entries[49].path.as_str()),
            ("/70", "/119")
        );
        let all = data(entries, 0, None, 200);
        assert_eq!((all.entries.len(), all.truncated), (120, false));
        let mut out = Vec::new();
        NetLogOutcome { data: d }
            .render_human(&mut out, &Style::default())
            .unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(
            text.contains("(70 earlier requests: --limit N or --since)"),
            "{text}"
        );
    }
}
