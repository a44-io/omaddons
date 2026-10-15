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

/// `net-log`'s own flags.
#[derive(Debug, Clone, Default, clap::Args)]
pub struct NetLogArgs {
    /// Only requests since a duration ago (30m, 12h, 1d, 2w) or an RFC 3339 time
    #[arg(long, value_name = "WHEN")]
    pub since: Option<String>,
}

/// `net-log`'s data.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct NetLogData {
    /// Oldest first.
    pub entries: Vec<NetLogEntry>,
    pub summary: Summary,
    /// Lines that do not parse.
    pub skipped: usize,
    #[schemars(with = "Option<String>")]
    pub since: Option<Timestamp>,
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
        data: NetLogData {
            summary: Summary::of(&read.entries),
            entries: read.entries,
            skipped: read.skipped,
            since,
        },
    })
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
}
