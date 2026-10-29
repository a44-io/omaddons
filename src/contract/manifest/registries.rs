//! The manifest's registries (M4-D56): every enumerable fact a consumer may switch on.
//! Error codes with their exits and retryability, warning codes, actions, events, phases,
//! log levels, the general exit dictionary, the `release` environment variables, and every
//! persisted kind with its own version. Meanings and descriptions are prose and are not
//! recorded.
//!
//! Not recorded yet, because their registries are not in this build: the doctor's exit
//! dictionary and each code's `doctor_exit` (E3), failure-mode ids (E1) and the doctor's
//! allowed operations (E4).

use std::collections::BTreeMap;

use super::ids::{self, section};
use super::{Builder, Fact, NodeKind, fact};
use crate::contract::actions::Action;
use crate::contract::env;
use crate::contract::events::{EventName, LogLevel, Phase};
use crate::engine::journal;
use crate::error::{ErrorCode, Exit, Retryability, WarningCode};
use crate::state::store::{KINDS, SchemaVersion};

/// The folder journal's kind, outside `KINDS`.
const JOURNAL: &str = "journal";

pub(crate) fn collect(b: &mut Builder) {
    for code in ErrorCode::ALL {
        let facts = [
            (fact::EXIT, Some(Fact::Int(i64::from(code.exit().code())))),
            (
                fact::RETRYABLE,
                Some(Fact::Text(retryable(code.retryability()).to_owned())),
            ),
        ];
        b.insert(
            ids::anchored(section::ERROR, code.wire(), None),
            NodeKind::ErrorCode,
            facts,
        );
    }
    for code in WarningCode::ALL {
        b.insert(
            ids::registry(section::WARNING, code.wire()),
            NodeKind::WarningCode,
            [],
        );
    }
    for action in Action::ALL {
        let facts = [(fact::DESTRUCTIVE, Some(Fact::Bool(action.destructive())))];
        b.insert(
            ids::registry(section::ACTION, action.wire()),
            NodeKind::Action,
            facts,
        );
    }
    for name in EventName::ALL {
        b.insert(
            ids::anchored(section::EVENT, name.wire(), None),
            NodeKind::Event,
            [],
        );
    }
    for phase in Phase::ALL {
        b.insert(
            ids::registry(section::PHASE, phase.wire()),
            NodeKind::Phase,
            [],
        );
    }
    for level in LogLevel::ALL {
        b.insert(
            ids::registry(section::LOG_LEVEL, level.wire()),
            NodeKind::LogLevel,
            [],
        );
    }
    for exit in Exit::ALL {
        let facts = [
            (fact::NAME, Some(Fact::Text(exit.name().to_owned()))),
            (
                fact::RETRYABLE,
                exit.retryable()
                    .map(|retryable| Fact::Text(retryable.to_string())),
            ),
        ];
        let id = ids::registry(section::EXIT, &format!("general {}", exit.code()));
        b.insert(id, NodeKind::ExitCode, facts);
    }
    for var in env::release() {
        let facts = [(
            fact::MIRRORS,
            var.mirrors.map(|flag| Fact::Text(flag.to_owned())),
        )];
        b.insert(
            ids::registry(section::ENV, var.name),
            NodeKind::EnvVar,
            facts,
        );
    }
    for (kind, version) in persisted_versions() {
        let facts = [(fact::VERSION, Some(Fact::Text(version.to_string())))];
        b.insert(
            ids::anchored(section::FILE, kind, None),
            NodeKind::FileKind,
            facts,
        );
    }
}

/// `true`, `false` or `conditional`, as `robot-docs errors` publishes it.
fn retryable(retryability: Retryability) -> &'static str {
    match retryability {
        Retryability::Fixed(true) => "true",
        Retryability::Fixed(false) => "false",
        Retryability::Conditional(_) => "conditional",
    }
}

/// Every persisted kind's own version: each `KINDS` row, and the folder journal.
pub(crate) fn persisted_versions() -> BTreeMap<&'static str, SchemaVersion> {
    KINDS
        .iter()
        .map(|row| (row.kind, row.current))
        .chain([(JOURNAL, journal::CURRENT)])
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::contract::Registry;
    use crate::contract::env::{ENV_VARS, EnvKind};
    use crate::contract::manifest::{Node, build_checked};
    use crate::contract::schema;

    fn nodes() -> BTreeMap<String, Node> {
        build_checked(&Registry::builtin()).0.nodes
    }

    fn get<'n>(nodes: &'n BTreeMap<String, Node>, id: &str, name: &str) -> Option<&'n Fact> {
        nodes.get(id).and_then(|node| node.facts.get(name))
    }

    /// The ids of `kind`'s nodes.
    fn of_kind(nodes: &BTreeMap<String, Node>, kind: NodeKind) -> BTreeSet<String> {
        nodes
            .iter()
            .filter(|(_, node)| node.kind == kind.wire())
            .map(|(id, _)| id.clone())
            .collect()
    }

    #[test]
    fn every_error_code_has_its_node() {
        let nodes = nodes();
        assert_eq!(
            of_kind(&nodes, NodeKind::ErrorCode).len(),
            ErrorCode::ALL.len()
        );
        let conditional: BTreeSet<&str> = ErrorCode::CONDITIONALLY_RETRYABLE
            .iter()
            .map(|c| c.wire())
            .collect();
        for code in ErrorCode::ALL {
            let id = ids::anchored(section::ERROR, code.wire(), None);
            let exit = get(&nodes, &id, fact::EXIT);
            assert_eq!(
                exit,
                Some(&Fact::Int(i64::from(code.exit().code()))),
                "{id} has {exit:?}"
            );
            let got = get(&nodes, &id, fact::RETRYABLE);
            let want = match code.retryability() {
                _ if conditional.contains(code.wire()) => "conditional",
                Retryability::Fixed(true) => "true",
                _ => "false",
            };
            assert_eq!(
                got,
                Some(&Fact::Text(want.into())),
                "{id} has {got:?}, expected {want:?}"
            );
        }
    }

    #[test]
    fn every_warning_code_action_event_phase_and_log_level_has_its_node() {
        let nodes = nodes();
        let expect = |kind: NodeKind, want: BTreeSet<String>| {
            assert_eq!(of_kind(&nodes, kind), want, "{kind:?}");
        };
        expect(
            NodeKind::WarningCode,
            WarningCode::ALL
                .iter()
                .map(|c| ids::registry(section::WARNING, c.wire()))
                .collect(),
        );
        expect(
            NodeKind::Action,
            Action::ALL
                .iter()
                .map(|a| ids::registry(section::ACTION, a.wire()))
                .collect(),
        );
        expect(
            NodeKind::Event,
            EventName::ALL
                .iter()
                .map(|e| ids::anchored(section::EVENT, e.wire(), None))
                .collect(),
        );
        expect(
            NodeKind::Phase,
            Phase::ALL
                .iter()
                .map(|p| ids::registry(section::PHASE, p.wire()))
                .collect(),
        );
        expect(
            NodeKind::LogLevel,
            LogLevel::ALL
                .iter()
                .map(|l| ids::registry(section::LOG_LEVEL, l.wire()))
                .collect(),
        );
        for action in Action::ALL {
            let id = ids::registry(section::ACTION, action.wire());
            assert_eq!(
                get(&nodes, &id, fact::DESTRUCTIVE),
                Some(&Fact::Bool(action.destructive())),
                "{id}"
            );
        }
    }

    #[test]
    fn general_exit_dictionary_is_recorded() {
        let nodes = nodes();
        assert_eq!(of_kind(&nodes, NodeKind::ExitCode).len(), Exit::ALL.len());
        for exit in Exit::ALL {
            let id = ids::registry(section::EXIT, &format!("general {}", exit.code()));
            assert_eq!(
                get(&nodes, &id, fact::NAME),
                Some(&Fact::Text(exit.name().into())),
                "{id}"
            );
            let want = exit.retryable().map(|r| Fact::Text(r.to_string()));
            assert_eq!(get(&nodes, &id, fact::RETRYABLE), want.as_ref(), "{id}");
        }
        assert_eq!(
            get(&nodes, "exit general 0", fact::RETRYABLE),
            None,
            "ok has no retryability"
        );
        assert_eq!(
            get(&nodes, "exit general 1", fact::RETRYABLE),
            None,
            "failed's errors decide"
        );
    }

    #[test]
    fn only_release_env_vars_are_recorded() {
        let nodes = nodes();
        for var in ENV_VARS {
            let id = ids::registry(section::ENV, var.name);
            let present = nodes.contains_key(&id);
            assert_eq!(present, var.kind == EnvKind::Release, "{id}");
            if present {
                let want = var.mirrors.map(|m| Fact::Text(m.to_owned()));
                assert_eq!(get(&nodes, &id, fact::MIRRORS), want.as_ref(), "{id}");
            }
        }
    }

    #[test]
    fn every_kinds_row_and_the_journal_has_a_file_node() {
        let nodes = nodes();
        assert_eq!(of_kind(&nodes, NodeKind::FileKind).len(), KINDS.len() + 1);
        for row in KINDS {
            let id = ids::anchored(section::FILE, row.kind, None);
            assert_eq!(
                get(&nodes, &id, fact::VERSION),
                Some(&Fact::Text(row.current.to_string())),
                "{id}"
            );
        }
        let journal_version = get(&nodes, "file journal", fact::VERSION);
        assert_eq!(
            journal_version,
            Some(&Fact::Text(journal::CURRENT.to_string()))
        );
    }

    #[test]
    fn persisted_versions_cover_every_file_schema() {
        let versions: BTreeSet<&str> = persisted_versions().into_keys().collect();
        let schemas: BTreeSet<&str> = schema::file_schemas()
            .into_keys()
            .filter(|kind| *kind != "desired_state")
            .collect();
        assert_eq!(versions, schemas);
    }
}
