//! The running policy (vision §5.5, D30): what may change while a target's client
//! runs, and what the player must do afterwards. Pure functions; whether the client
//! runs is decided live by `discover::running` and passed in. When the process table
//! could not be read, every client counts as running (fail closed), and a refusal says
//! it "may be running" (`details.running: "unknown"`).
//!
//! A Linux rename never disturbs files the client has open. What a running client picks
//! up: a loaded addon's changed TOC on `/reload` (observed on 3.3.5a on 2026-10-20;
//! assumed on modern clients from the vision's sources), which a rules row says
//! (`reload_rereads_toc`); deleting or renaming a loaded TOC needs a restart. A folder
//! added while the client runs was observed only on 3.3.5a, where three `/reload`s did
//! not show it, and never on a modern client, so a new folder reports `restart` on every
//! client until one is observed loading on `/reload` (omad-launch-week-fixes-bmcx.5: a
//! needless restart costs less than an install that looks broken). SavedVariables are
//! never written while the client runs, under any policy or flag (D33): the client
//! overwrites them on logout.
//!
//! - `safe` (default): adds and TOC-preserving replacements are allowed; removing a
//!   loaded folder and dropping its loaded TOC are refused with `game_running` (exit 4).
//!   On a legacy client only a TOC-preserving replacement is allowed, and only where the
//!   row re-reads TOCs (3.3.5a); every other change waits for a restart.
//! - `refuse`: every folder change is refused. `allow`: every folder change is allowed.
//! - `--while-running` overrides every folder rule, never the SavedVariables rule.

use std::collections::BTreeSet;

use serde_json::json;

use crate::error::{Error, ErrorCode, Result, Suggestion, Warning, WarningCode};
use crate::model::{FolderName, ReloadRequired};
use crate::rules::ClientRules;
use crate::state::config::RunningPolicy;

/// A planned change, with what the policy needs to judge it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyOp {
    AddFolder {
        folder: FolderName,
    },
    ReplaceFolder {
        folder: FolderName,
        /// The TOC file the client loads from the live folder now, if any.
        loaded_toc_before: Option<String>,
        /// The TOC file names of the new version.
        toc_names_after: BTreeSet<String>,
    },
    RemoveFolder {
        folder: FolderName,
        loaded_toc_before: Option<String>,
    },
    /// Undo (and later `backup restore`) writing SavedVariables back.
    RestoreSavedVariables {
        files: usize,
    },
}

impl PolicyOp {
    /// Whether the op deletes or renames a TOC the client has loaded.
    fn drops_loaded_toc(&self) -> bool {
        match self {
            Self::ReplaceFolder {
                loaded_toc_before: Some(loaded),
                toc_names_after,
                ..
            } => !toc_names_after
                .iter()
                .any(|t| t.eq_ignore_ascii_case(loaded)),
            Self::RemoveFolder {
                loaded_toc_before: Some(_),
                ..
            } => true,
            _ => false,
        }
    }

    fn describe(&self) -> serde_json::Value {
        match self {
            Self::AddFolder { folder } => json!({ "op": "add", "folder": folder.as_str() }),
            Self::ReplaceFolder { folder, .. } => {
                json!({ "op": "replace", "folder": folder.as_str() })
            }
            Self::RemoveFolder { folder, .. } => {
                json!({ "op": "remove", "folder": folder.as_str() })
            }
            Self::RestoreSavedVariables { files } => {
                json!({ "op": "restore_saved_variables", "files": files })
            }
        }
    }
}

/// Who is asking: the target's id for messages, and the argv to repeat in fixes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyContext<'a> {
    pub target_id: &'a str,
    pub argv: &'a [String],
    /// Why the process table could not be read, when it could not: "running" then means
    /// "may be running", and a refusal says so (`details.running: "unknown"`).
    pub running_unknown: Option<&'a str>,
}

impl PolicyContext<'_> {
    /// "retail's game client is running", or "may be running" with the cause.
    fn is_running(&self) -> String {
        match self.running_unknown {
            Some(cause) => format!("{}'s game client may be running ({cause})", self.target_id),
            None => format!("{}'s game client is running", self.target_id),
        }
    }

    /// `details` of a refusal, with `running: "unknown"` when that is the case.
    fn details(&self, mut details: serde_json::Value) -> serde_json::Value {
        if self.running_unknown.is_some()
            && let Some(map) = details.as_object_mut()
        {
            map.insert("running".into(), "unknown".into());
        }
        details
    }
}

/// What the player must do once the change is in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyOutcome {
    pub reload_required: ReloadRequired,
    /// `reload_required` when it is not `none`.
    pub warnings: Vec<Warning>,
}

/// Judges `ops` for one target. `Err(game_running)` when anything is refused.
pub fn evaluate(
    ctx: &PolicyContext<'_>,
    policy: RunningPolicy,
    running: bool,
    rules: &ClientRules,
    ops: &[PolicyOp],
    while_running: bool,
) -> Result<PolicyOutcome> {
    if !running {
        return Ok(PolicyOutcome {
            reload_required: ReloadRequired::None,
            warnings: Vec::new(),
        });
    }
    let target = ctx.target_id;
    if ops
        .iter()
        .any(|op| matches!(op, PolicyOp::RestoreSavedVariables { .. }))
    {
        return Err(Error::new(
            ErrorCode::GameRunning,
            format!("{}, and SavedVariables are never written while it runs (it overwrites them on logout)", ctx.is_running()),
            Suggestion::text("quit the game client, then rerun"),
        )
        .details(ctx.details(json!({
            "target": target,
            "reason": "saved_variables",
            "ops": ops.iter().filter(|op| matches!(op, PolicyOp::RestoreSavedVariables { .. })).map(PolicyOp::describe).collect::<Vec<_>>(),
        }))));
    }

    let (refused, reason): (Vec<&PolicyOp>, &str) = match (policy, while_running) {
        (_, true) | (RunningPolicy::Allow, _) => (Vec::new(), ""),
        (RunningPolicy::Refuse, false) => (ops.iter().collect(), "policy_refuse"),
        (RunningPolicy::Safe, false) => {
            let refused: Vec<&PolicyOp> = ops.iter().filter(|op| !safe_lands(op, rules)).collect();
            // A client that re-reads TOCs refuses only what /reload cannot pick up.
            if rules.reload_rereads_toc && refused.iter().all(|op| op.drops_loaded_toc()) {
                (refused, "loaded_toc")
            } else {
                (refused, "legacy_client")
            }
        }
    };
    if !refused.is_empty() {
        let folders = refused
            .iter()
            .filter_map(|op| folder(op))
            .collect::<Vec<_>>()
            .join(", ");
        let why = match reason {
            "policy_refuse" => "running_policy is refuse".to_owned(),
            "legacy_client" if rules.reload_rereads_toc => {
                let adds = refused
                    .iter()
                    .all(|op| matches!(op, PolicyOp::AddFolder { .. }));
                // A replacement is refused only for the loaded TOC it drops.
                let drops = refused.iter().any(|op| op.drops_loaded_toc());
                let what = match (adds, drops) {
                    (true, _) => "a new addon folder",
                    (false, false) => "an addon folder added or removed",
                    (false, true) => {
                        "an addon folder added or removed, or a loaded TOC removed or renamed"
                    }
                };
                format!(
                    "this client needs a restart to see {what} ({folders}); a replacement that keeps its loaded TOC needs only /reload"
                )
            }
            "legacy_client" => "this client needs a restart to pick up any addon change".to_owned(),
            _ => format!("{folders} would remove or rename a TOC the client has loaded"),
        };
        // Runnable as is: without the failing run's presentation flags.
        let mut retry = crate::cli::fix_argv(ctx.argv);
        retry.push("--while-running".to_owned());
        return Err(Error::new(
            ErrorCode::GameRunning,
            format!("{}: {why}", ctx.is_running()),
            Suggestion::command(retry),
        )
        .alternative(Suggestion::text("quit the game client, then rerun"))
        .details(ctx.details(json!({
            "target": target,
            "reason": reason,
            "ops": refused.iter().map(|op| op.describe()).collect::<Vec<_>>(),
        }))));
    }

    let reload_required = ops
        .iter()
        .map(|op| match op {
            op if op.drops_loaded_toc() => ReloadRequired::Restart,
            // Never observed loading on `/reload` (see the module comment).
            PolicyOp::AddFolder { .. } => ReloadRequired::Restart,
            PolicyOp::ReplaceFolder { .. } if rules.reload_rereads_toc => ReloadRequired::Reload,
            PolicyOp::ReplaceFolder { .. } => ReloadRequired::Restart,
            _ if rules.legacy => ReloadRequired::Restart,
            PolicyOp::RemoveFolder { .. } => ReloadRequired::None,
            _ => ReloadRequired::Reload,
        })
        .max()
        .unwrap_or_default();
    let warnings = match reload_required {
        ReloadRequired::None => Vec::new(),
        ReloadRequired::Reload => vec![
            Warning::new(
                WarningCode::ReloadRequired,
                format!("{target}'s game client is running: type /reload to load the changes"),
            )
            .with_fix(Suggestion::text("/reload in game")),
        ],
        ReloadRequired::Restart => vec![
            Warning::new(
                WarningCode::ReloadRequired,
                format!("{target}'s game client is running: restart it to load the changes"),
            )
            .with_fix(Suggestion::text("restart the game client")),
        ],
    };
    Ok(PolicyOutcome {
        reload_required,
        warnings,
    })
}

/// Whether `safe` lets `op` land while the client runs: never a dropped loaded TOC; a
/// replacement when the client re-reads TOCs on `/reload`; an add or a remove unless
/// the client is legacy, which sees folders come and go only after a restart.
fn safe_lands(op: &PolicyOp, rules: &ClientRules) -> bool {
    match op {
        op if op.drops_loaded_toc() => false,
        PolicyOp::ReplaceFolder { .. } => rules.reload_rereads_toc,
        PolicyOp::AddFolder { .. } | PolicyOp::RemoveFolder { .. } => !rules.legacy,
        // Refused before any folder rule.
        PolicyOp::RestoreSavedVariables { .. } => false,
    }
}

fn folder(op: &PolicyOp) -> Option<&str> {
    match op {
        PolicyOp::AddFolder { folder }
        | PolicyOp::ReplaceFolder { folder, .. }
        | PolicyOp::RemoveFolder { folder, .. } => Some(folder.as_str()),
        PolicyOp::RestoreSavedVariables { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The client a judgement is for.
    #[derive(Debug, Clone, Copy)]
    enum Client {
        Modern,
        /// Legacy, needing a restart for every change (1.12.1, 2.4.3, 4.3.4, 5.4.8).
        Legacy,
        /// 3.3.5a: legacy, but a loaded TOC is re-read on `/reload`.
        Wrath,
    }

    fn rules(client: Client) -> ClientRules {
        ClientRules {
            legacy: !matches!(client, Client::Modern),
            reload_rereads_toc: !matches!(client, Client::Legacy),
            ..ClientRules::default()
        }
    }

    fn name(n: &str) -> FolderName {
        FolderName::new(n).unwrap()
    }

    fn add() -> PolicyOp {
        PolicyOp::AddFolder {
            folder: name("New"),
        }
    }

    fn replace(kept: bool) -> PolicyOp {
        let after = if kept { "foo.toc" } else { "Foo.toc" };
        PolicyOp::ReplaceFolder {
            folder: name("Foo"),
            loaded_toc_before: Some(if kept { "Foo.toc" } else { "Foo_Mainline.toc" }.into()),
            toc_names_after: BTreeSet::from([after.to_owned()]),
        }
    }

    fn remove(loaded: bool) -> PolicyOp {
        PolicyOp::RemoveFolder {
            folder: name("Old"),
            loaded_toc_before: loaded.then(|| "Old.toc".to_owned()),
        }
    }

    const ARGV: &[&str] = &["omaddons", "remove", "Old", "-t", "retail"];

    fn run(
        policy: RunningPolicy,
        running: bool,
        client: Client,
        ops: &[PolicyOp],
        while_running: bool,
    ) -> Result<PolicyOutcome> {
        let argv: Vec<String> = ARGV.iter().map(|s| (*s).to_owned()).collect();
        let ctx = PolicyContext {
            target_id: "retail",
            argv: &argv,
            running_unknown: None,
        };
        evaluate(&ctx, policy, running, &rules(client), ops, while_running)
    }

    /// (policy, running, client, op, --while-running, `Ok(reload)` or `Err(reason)`)
    type Row = (
        RunningPolicy,
        bool,
        Client,
        PolicyOp,
        bool,
        std::result::Result<ReloadRequired, String>,
    );

    /// `Ok(reload)` or `Err(reason)`.
    fn verdict(result: Result<PolicyOutcome>) -> std::result::Result<ReloadRequired, String> {
        match result {
            Ok(outcome) => Ok(outcome.reload_required),
            Err(e) => {
                assert_eq!((e.code, e.exit().code()), (ErrorCode::GameRunning, 4));
                Err(e.details["reason"].as_str().unwrap_or_default().to_owned())
            }
        }
    }

    #[test]
    fn the_policy_table() {
        use Client::{Legacy, Modern, Wrath};
        use ReloadRequired::{None as N, Reload as R, Restart as X};
        use RunningPolicy::{Allow, Refuse, Safe};
        let refused = |r: &str| Err(r.to_owned());
        #[rustfmt::skip]
        let rows: Vec<Row> = vec![
            // Not running: everything goes, nothing to do in game.
            (Safe, false, Modern, remove(true), false, Ok(N)),
            (Refuse, false, Legacy, add(), false, Ok(N)),
            (Allow, false, Modern, replace(false), false, Ok(N)),
            // Safe, modern client. A new folder lands, then asks for a restart: no client
            // was ever observed loading one on /reload (bmcx.5).
            (Safe, true, Modern, add(), false, Ok(X)),
            (Safe, true, Modern, replace(true), false, Ok(R)),
            (Safe, true, Modern, replace(false), false, refused("loaded_toc")),
            (Safe, true, Modern, replace(false), true, Ok(X)),
            (Safe, true, Modern, remove(true), false, refused("loaded_toc")),
            (Safe, true, Modern, remove(true), true, Ok(X)),
            (Safe, true, Modern, remove(false), false, Ok(N)),
            // Safe, legacy client: every change waits for a restart.
            (Safe, true, Legacy, add(), false, refused("legacy_client")),
            (Safe, true, Legacy, replace(true), false, refused("legacy_client")),
            (Safe, true, Legacy, remove(true), false, refused("legacy_client")),
            (Safe, true, Legacy, add(), true, Ok(X)),
            (Safe, true, Legacy, replace(true), true, Ok(X)),
            // Safe, 3.3.5a: a replacement keeping its loaded TOC lands on /reload.
            (Safe, true, Wrath, replace(true), false, Ok(R)),
            (Safe, true, Wrath, add(), false, refused("legacy_client")),
            (Safe, true, Wrath, remove(false), false, refused("legacy_client")),
            (Safe, true, Wrath, remove(true), false, refused("loaded_toc")),
            (Safe, true, Wrath, replace(false), false, refused("loaded_toc")),
            (Safe, true, Wrath, add(), true, Ok(X)),
            // Refuse.
            (Refuse, true, Modern, add(), false, refused("policy_refuse")),
            (Refuse, true, Modern, replace(true), true, Ok(R)),
            (Refuse, true, Wrath, replace(true), false, refused("policy_refuse")),
            // Allow (a new folder: restart, as above).
            (Allow, true, Modern, add(), false, Ok(X)),
            (Allow, true, Modern, remove(true), false, Ok(X)),
            (Allow, true, Legacy, add(), false, Ok(X)),
            (Allow, true, Legacy, replace(true), false, Ok(X)),
            (Allow, true, Wrath, replace(true), false, Ok(R)),
            (Allow, true, Wrath, remove(false), false, Ok(X)),
        ];
        for (policy, running, client, op, while_running, want) in rows {
            let label = format!(
                "{policy:?} running={running} {client:?} {op:?} while_running={while_running}"
            );
            assert_eq!(
                verdict(run(policy, running, client, &[op], while_running)),
                want,
                "{label}"
            );
        }
    }

    #[test]
    fn saved_variables_are_never_written_while_running() {
        let restore = PolicyOp::RestoreSavedVariables { files: 3 };
        for policy in [
            RunningPolicy::Safe,
            RunningPolicy::Refuse,
            RunningPolicy::Allow,
        ] {
            for while_running in [false, true] {
                let err = run(
                    policy,
                    true,
                    Client::Modern,
                    &[restore.clone(), add()],
                    while_running,
                )
                .unwrap_err();
                assert_eq!(err.details["reason"], "saved_variables");
                assert_eq!(
                    (err.fix.display(), err.fix.argv()),
                    ("quit the game client, then rerun", None)
                );
            }
        }
        let stopped = run(
            RunningPolicy::Safe,
            false,
            Client::Modern,
            &[restore],
            false,
        );
        assert!(stopped.is_ok());
    }

    #[test]
    fn a_while_running_retry_drops_the_failing_runs_presentation_flags() {
        let shown: Vec<String> = ["omaddons", "remove", "Foo", "--json", "--format", "json"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        let ctx = PolicyContext {
            target_id: "retail",
            argv: &shown,
            running_unknown: None,
        };
        let err = evaluate(
            &ctx,
            RunningPolicy::Safe,
            true,
            &rules(Client::Modern),
            &[add(), remove(true)],
            false,
        )
        .unwrap_err();
        assert_eq!(
            err.fix.argv().unwrap(),
            ["omaddons", "remove", "Foo", "--while-running"]
        );
    }

    #[test]
    fn a_refusal_offers_while_running_and_quitting() {
        let err = run(
            RunningPolicy::Safe,
            true,
            Client::Modern,
            &[add(), remove(true)],
            false,
        )
        .unwrap_err();
        let mut want: Vec<String> = ARGV.iter().map(|s| (*s).to_owned()).collect();
        want.push("--while-running".into());
        assert_eq!(err.fix.argv(), Some(want.as_slice()));
        assert_eq!(
            err.alternatives[0].display(),
            "quit the game client, then rerun"
        );
        assert_eq!(
            err.details["ops"],
            json!([{ "op": "remove", "folder": "Old" }]),
            "only the refused op"
        );
        assert!(
            err.message.contains("retail") && err.message.contains("Old"),
            "{}",
            err.message
        );
    }

    /// On 3.3.5a the refusal names what a restart shows and says a replacement would land;
    /// the other legacy clients keep their message.
    #[test]
    fn a_legacy_refusal_says_what_needs_the_restart() {
        let ops = [replace(true), add()];
        let err = run(RunningPolicy::Safe, true, Client::Wrath, &ops, false).unwrap_err();
        assert!(
            err.message.ends_with(
                "this client needs a restart to see a new addon folder (New); a replacement that keeps its loaded TOC needs only /reload"
            ),
            "{}",
            err.message
        );
        assert_eq!(
            err.details["ops"],
            json!([{ "op": "add", "folder": "New" }])
        );
        let err = run(RunningPolicy::Safe, true, Client::Legacy, &ops, false).unwrap_err();
        assert!(
            err.message
                .ends_with("this client needs a restart to pick up any addon change"),
            "{}",
            err.message
        );
        // A replacement refused beside a new folder is refused for the TOC it drops.
        let mixed = [replace(false), add()];
        let err = run(RunningPolicy::Safe, true, Client::Wrath, &mixed, false).unwrap_err();
        assert!(
            err.message.contains(
                "to see an addon folder added or removed, or a loaded TOC removed or renamed (Foo, New)"
            ),
            "{}",
            err.message
        );
    }

    #[test]
    fn the_strongest_requirement_wins_and_is_warned() {
        let outcome = run(
            RunningPolicy::Allow,
            true,
            Client::Modern,
            &[add(), remove(false), replace(false)],
            false,
        )
        .unwrap();
        assert_eq!(outcome.reload_required, ReloadRequired::Restart);
        assert_eq!(outcome.warnings.len(), 1);
        assert_eq!(
            (
                outcome.warnings[0].code,
                outcome.warnings[0].fix.as_ref().map(Suggestion::display)
            ),
            (WarningCode::ReloadRequired, Some("restart the game client"))
        );
        // A replacement keeping its loaded TOC: /reload.
        let outcome = run(
            RunningPolicy::Safe,
            true,
            Client::Modern,
            &[replace(true)],
            false,
        )
        .unwrap();
        assert_eq!(
            outcome.warnings[0]
                .fix
                .as_ref()
                .map(|f| (f.display(), f.argv())),
            Some(("/reload in game", None))
        );
        // A new folder, alone or beside such a replacement: a restart (bmcx.5).
        for ops in [vec![add()], vec![add(), replace(true)]] {
            let outcome = run(RunningPolicy::Safe, true, Client::Modern, &ops, false).unwrap();
            assert_eq!(
                (
                    outcome.reload_required,
                    outcome.warnings[0].fix.as_ref().map(Suggestion::display)
                ),
                (ReloadRequired::Restart, Some("restart the game client")),
                "{ops:?}"
            );
        }
        assert!(
            run(RunningPolicy::Safe, true, Client::Modern, &[], false)
                .unwrap()
                .warnings
                .is_empty()
        );
    }

    #[test]
    fn renaming_the_loaded_toc_drops_it() {
        let rename = PolicyOp::ReplaceFolder {
            folder: name("Foo"),
            loaded_toc_before: Some("Foo_Mainline.toc".into()),
            toc_names_after: BTreeSet::from(["Foo.toc".to_owned()]),
        };
        assert!(rename.drops_loaded_toc());
        let kept_other_case = PolicyOp::ReplaceFolder {
            folder: name("Foo"),
            loaded_toc_before: Some("Foo_Mainline.toc".into()),
            toc_names_after: BTreeSet::from(["foo_mainline.TOC".to_owned(), "Foo.toc".to_owned()]),
        };
        assert!(!kept_other_case.drops_loaded_toc());
        let nothing_loaded = PolicyOp::ReplaceFolder {
            folder: name("Foo"),
            loaded_toc_before: None,
            toc_names_after: BTreeSet::new(),
        };
        assert!(!nothing_loaded.drops_loaded_toc());
    }
}
