//! Recommended next commands: one registry owns their names, meanings and defaults.

use std::fmt;

use schemars::JsonSchema;
use serde::Serialize;

macro_rules! actions {
    ($($(#[$doc:meta])* $variant:ident = $wire:literal, $destructive:literal, $description:literal;)+) => {
        /// A recommended next command's `action` (`commands[].action`, M4-D11).
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, JsonSchema)]
        pub enum Action { $($(#[$doc])* #[serde(rename = $wire)] $variant,)+ }

        impl Action {
            /// Every action, in declaration order.
            pub const ALL: &[Action] = &[$(Self::$variant,)+];
            /// The snake_case wire name.
            pub const fn wire(self) -> &'static str { match self { $(Self::$variant => $wire,)+ } }
            /// One line of plain text, no trailing period.
            pub const fn description(self) -> &'static str { match self { $(Self::$variant => $description,)+ } }
            /// Whether this action may be destructive (it removes addons, undoes a change,
            /// restores settings or prunes backups), its worst case: confirm before running
            /// one unless the hint's own `destructive`, the instance's value, says otherwise.
            pub const fn destructive(self) -> bool { match self { $(Self::$variant => $destructive,)+ } }
            /// The action named `name`, if one is.
            pub fn from_wire(name: &str) -> Option<Self> {
                Self::ALL.iter().copied().find(|a| a.wire() == name)
            }
            #[cfg(test)]
            const fn rust_name(self) -> &'static str { match self { $(Self::$variant => stringify!($variant),)+ } }
        }
    };
}

actions! {
    /// Offered after every committed change, for one transaction: it removes what the
    /// change added and puts back what it replaced, settings included.
    Undo = "undo", true, "Undo one transaction";
    /// Offered after a change on several clients.
    UndoRun = "undo_run", true, "Undo every transaction of one run";
    /// Offered by status, outdated and errors.
    Update = "update", false, "Install the available updates";
    /// Offered by status when an update check is due.
    CheckUpdates = "check_updates", false, "Check every client for updates";
    /// Offered by status, install and the adopt report.
    Adopt = "adopt", false, "Adopt folders installed by hand";
    /// Offered by the adopt report for a guess.
    AdoptFolder = "adopt_folder", false, "Adopt one folder identified only by a guess";
    /// Offered by the adopt report for a guess with rivals.
    AdoptWithRef = "adopt_with_ref", false, "Adopt one folder from a chosen source";
    /// Offered for search results and unresolved or bridge dependencies.
    Install = "install", false, "Install an addon";
    /// Offered for a bridge dependency nothing resolves.
    Search = "search", false, "Search the sources for an addon";
    /// Offered by search for a result no selected client has a verdict for: info judges
    /// each client from the newest release, where search guesses nothing.
    Info = "info", false, "Show which clients an addon's newest release likely loads on";
    /// Offered for a managed addon with drift.
    Reinstall = "reinstall", false, "Reinstall an addon whose files were changed";
    /// Offered by status for a degraded addon.
    Inspect = "inspect", false, "Show a client's addons in detail";
    /// Offered by cached status when no cache exists.
    Status = "status", false, "Run a full status, which builds the cache";
    /// Offered by status for orphaned dependencies.
    RemoveOrphans = "remove_orphans", true, "Remove dependencies nothing requires any more";
    /// Offered by status for unmanaged folders.
    ListUnmanaged = "list_unmanaged", false, "List the folders omaddons does not manage";
    /// Offered by status for a missing client.
    ForgetTarget = "forget_target", true, "Forget a client that is no longer on this machine";
    /// Offered by status and backup list for an unprotected client.
    BackupCreate = "backup_create", false, "Back up a client's settings";
    /// Offered by status for a stale backup inventory.
    RefreshBackups = "refresh_backups", false, "Refresh a client's backup summary";
    /// Offered by cached status for stale or unchecked Lua errors.
    CheckErrors = "check_errors", false, "Read a client's Lua errors";
    /// Offered by errors for the current observation.
    DismissErrors = "dismiss_errors", false, "Dismiss a client's current Lua errors";
    /// Offered by errors when a BugGrabber build is available.
    InstallBuggrabber = "install_buggrabber", false, "Install BugGrabber so Lua errors are captured";
    /// Offered by status and omarchy status for an outdated widget.
    ReinstallWidget = "reinstall_widget", false, "Bring the bar widget up to date";
    /// Offered by omarchy status for a widget absent from the bar.
    EnableWidget = "enable_widget", false, "Put the omaddons widget on the bar";
    /// Offered by a dry run that stored its plan, and by apply's own dry run. Each hint is
    /// as destructive as its plan's command; the default is the worst case.
    Apply = "apply", true, "Run exactly what a dry run stored";
    /// Offered by doctor diagnose, once per failure mode it found.
    DoctorExplain = "doctor_explain", false, "Explain one failure mode and its instances";
    /// Offered by a doctor fix dry run that plans a repair, bound to its token.
    DoctorFix = "doctor_fix", false, "Run exactly the repair a doctor fix dry run showed";
    /// Offered by robot-docs install-skill, once per other agent's skill root that exists.
    InstallSkill = "install_skill", false, "Install the omaddons skill into another agent's skill folder";
    /// Offered after a change that marked releases to pass over (`skip`), scoped to that
    /// transaction's marks, so it never clears an independent decision about another release.
    Unskip = "unskip", false, "Offer again the releases one transaction marked skipped";
    /// Offered by `errors` on a link to the change before a new error (RG-40): one addon
    /// back to the files installed before that change, which it names (`skip --back --tx`).
    Rollback = "rollback", true, "Take one addon back to the files installed before a change";
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.wire())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::Path;

    use schemars::generate::SchemaSettings;
    use serde_json::Value;

    use super::*;
    use crate::source_scan::{Literals, production_code, sources};

    #[test]
    fn every_action_has_a_unique_snake_case_wire_name() {
        assert_eq!(
            Action::ALL.len(),
            29,
            "registered actions: {:?}",
            Action::ALL
        );
        let mut seen = BTreeSet::new();
        for action in Action::ALL {
            let wire = action.wire();
            assert!(
                wire.starts_with(|c: char| c.is_ascii_lowercase())
                    && wire
                        .bytes()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_'),
                "{action:?}: invalid wire {wire:?}"
            );
            assert!(seen.insert(wire), "{action:?}: duplicate wire {wire:?}");
            assert_eq!(action.to_string(), wire, "{action:?}: Display");
        }
    }

    #[test]
    fn serde_and_schema_names_equal_wire() {
        let mut generator = SchemaSettings::draft2020_12()
            .for_serialize()
            .into_generator();
        let schema = serde_json::to_value(generator.root_schema_for::<Action>()).unwrap();
        let values = schema["enum"].as_array().cloned().unwrap_or_else(|| {
            schema["oneOf"]
                .as_array()
                .unwrap()
                .iter()
                .map(|row| row["const"].clone())
                .collect()
        });
        let expected: BTreeSet<_> = Action::ALL.iter().map(|a| a.wire()).collect();
        let actual: BTreeSet<_> = values.iter().map(|v| v.as_str().unwrap()).collect();
        assert_eq!(actual, expected, "Action schema: {schema}");
        assert_eq!(
            values.len(),
            Action::ALL.len(),
            "Action schema has no duplicate names: {schema}"
        );
        for action in Action::ALL {
            assert_eq!(
                serde_json::to_value(action).unwrap(),
                Value::from(action.wire()),
                "{action:?}: serde wire"
            );
        }
    }

    #[test]
    fn from_wire_round_trips_and_rejects_unknown() {
        for action in Action::ALL {
            assert_eq!(
                Action::from_wire(action.wire()),
                Some(*action),
                "{action:?}: {}",
                action.wire()
            );
        }
        for (case, wire) in [
            ("empty", ""),
            ("capitalized", "Undo"),
            ("trailing_space", "undo "),
            ("unknown", "no_such_action"),
            ("a_command_not_an_action", "remove"),
        ] {
            assert_eq!(Action::from_wire(wire), None, "{case}: {wire:?}");
        }
    }

    #[test]
    fn descriptions_are_one_plain_line() {
        for action in Action::ALL {
            let description = action.description();
            assert!(
                description.starts_with(|c: char| c.is_ascii_uppercase())
                    && description.chars().count() <= 80
                    && !description.chars().any(char::is_control)
                    && !description.contains('`')
                    && !description.ends_with('.'),
                "{action:?}: invalid description {description:?}"
            );
        }
    }

    #[test]
    fn destructive_defaults_are_pinned() {
        let cases = [
            (Action::Undo, true),
            (Action::UndoRun, true),
            (Action::Update, false),
            (Action::CheckUpdates, false),
            (Action::Adopt, false),
            (Action::AdoptFolder, false),
            (Action::AdoptWithRef, false),
            (Action::Install, false),
            (Action::Search, false),
            (Action::Info, false),
            (Action::Reinstall, false),
            (Action::Inspect, false),
            (Action::Status, false),
            (Action::RemoveOrphans, true),
            (Action::ListUnmanaged, false),
            (Action::ForgetTarget, true),
            (Action::BackupCreate, false),
            (Action::RefreshBackups, false),
            (Action::CheckErrors, false),
            (Action::DismissErrors, false),
            (Action::InstallBuggrabber, false),
            (Action::ReinstallWidget, false),
            (Action::EnableWidget, false),
            (Action::Apply, true),
            (Action::DoctorExplain, false),
            (Action::DoctorFix, false),
            (Action::InstallSkill, false),
            (Action::Unskip, false),
            (Action::Rollback, true),
        ];
        for action in Action::ALL {
            assert!(
                cases.iter().any(|(a, _)| a == action),
                "add Action::{}'s destructive default to destructive_defaults_are_pinned",
                action.rust_name()
            );
        }
        assert_eq!(
            cases.iter().map(|(a, _)| *a).collect::<BTreeSet<_>>(),
            Action::ALL.iter().copied().collect(),
            "destructive cases cover ALL exactly"
        );
        for (action, expected) in cases {
            assert_eq!(
                action.destructive(),
                expected,
                "{action:?}: destructive default"
            );
        }
    }

    #[test]
    fn every_action_is_constructed_by_production_code() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let source: Vec<_> = sources(&root)
            .into_iter()
            .filter(|(path, _)| *path != root.join("contract/actions.rs"))
            .filter_map(|(path, text)| {
                production_code(&text, Literals::Blank).map(|code| (path, code))
            })
            .collect();
        for action in Action::ALL {
            let needle = format!("Action::{}", action.rust_name());
            let found = source.iter().any(|(_, code)| {
                code.match_indices(&needle).any(|(at, _)| {
                    code[at + needle.len()..]
                        .chars()
                        .next()
                        .is_none_or(|c| !c.is_ascii_alphanumeric() && c != '_')
                })
            });
            assert!(
                found,
                "{needle} is never constructed outside src/contract/actions.rs: remove it, or emit it from the command that offers it; searched {} files",
                source.len()
            );
        }
    }

    #[test]
    fn every_action_is_documented_in_readme() {
        let line = include_str!("../../README.md")
            .lines()
            .find(|line| line.starts_with("| `commands` |"))
            .unwrap();
        for action in Action::ALL {
            assert!(
                line.contains(&format!("`{}`", action.wire())),
                "{action:?}: document {} in README's commands row",
                action.wire()
            );
        }
    }
}
