//! Details declared from their emitters; shared wrapper keys are merged per code.

use std::collections::BTreeSet;

use serde_json::{Map, Value};

use super::{DetailSpec, ErrorCode, Field, Mismatch};

const STR: DetailSpec = DetailSpec::Str;
const INT: DetailSpec = DetailSpec::Int;
const BOOL: DetailSpec = DetailSpec::Bool;
const STRINGS: DetailSpec = DetailSpec::Array(&STR);
const FOLDER_CLASSES: DetailSpec = DetailSpec::Enum(&[
    "protected",
    "managed",
    "shared",
    "linked",
    "unmanaged",
    "invalid",
]);

/// `engine/ownership.rs::check` and `refused`, and `engine/plan.rs::takeover_refused`.
/// Within-transaction conflicts carry only `folder` and `owner`, so code and reason
/// are optional in this shared shape as well as the conditional evidence fields.
pub(crate) const REFUSED_FOLDER: DetailSpec = DetailSpec::Object(&[
    Field::opt("class", &FOLDER_CLASSES),
    Field::opt("code", &STR),
    Field::opt("existing", &DetailSpec::OneOf(&[&STR, &STRINGS])),
    Field::req("folder", &STR),
    Field::opt("owner", &STR),
    Field::opt("owner_target", &STR),
    Field::opt(
        "reason",
        &DetailSpec::Enum(&[
            "protected",
            "linked",
            "case_duplicates",
            "owned_on_shared_target",
            "case_collision",
            "owned_by_another",
            "unmanaged",
            "same_name_other_ref",
        ]),
    ),
    Field::opt("ref", &STR),
]);

/// `app/outcome.rs::ItemError`; its deliberately retained subset of details is opaque.
pub(crate) const ITEM_ERROR: DetailSpec = DetailSpec::Object(&[
    Field::req("code", &STR),
    Field::opt(
        "details",
        &DetailSpec::Opaque(
            "An item error's kept details: the subset of its own code's details an item keeps",
        ),
    ),
    Field::req("fix", &STR),
    Field::req("fix_argv", &STRINGS).nullable(),
    Field::req("message", &STR),
]);

/// `fsx/lock.rs::busy` serializes `HolderInfo`.
pub(crate) const HOLDER: DetailSpec = DetailSpec::Object(&[
    // A plan lease's holder (`apply`): how long it has held it, in whole seconds.
    Field::opt("age_s", &INT),
    Field::req("command", &STR),
    Field::req("pid", &INT),
    Field::req("started_at", &STR),
]);

/// `engine/plan.rs` candidate attempts: reason includes interpolated prose.
pub(crate) const TRIED: DetailSpec = DetailSpec::Array(&DetailSpec::Object(&[
    Field::req("asset", &STR),
    Field::req("reason", &STR),
]));

/// `app/install.rs::all_failed`, `app/outcome.rs::all_failed`,
/// `engine/plan.rs` dependency failures and `engine/tx.rs::recovery_failed`.
pub(crate) const COMMON: DetailSpec = DetailSpec::Object(&[
    Field::opt(
        "items",
        &DetailSpec::Array(&DetailSpec::OneOf(&[
            &DetailSpec::Object(&[
                Field::req("code", &STR),
                Field::req("message", &STR),
                Field::req("request", &STR),
            ]),
            &DetailSpec::Object(&[
                Field::req("error", &ITEM_ERROR).nullable(),
                Field::req("request", &STR),
                Field::req("target", &STR),
            ]),
        ])),
    ),
    Field::opt("journal_pending", &STR),
    Field::opt(
        "recovery_error",
        &DetailSpec::Object(&[Field::req("code", &STR), Field::req("message", &STR)]),
    ),
    Field::opt("required_by", &STRINGS),
]);

/// `cli.rs::misplaced`, `discover/registry.rs::validate_name`, and backup refusals.
pub(crate) const USAGE: DetailSpec = DetailSpec::Object(&[
    Field::opt("command", &STR),
    // `apply`'s refusal of a flag the plan already decides (`--target`, `--while-running`).
    Field::opt("flag", &STR),
    Field::opt("holder", &STR),
    Field::opt("id", &STR),
    Field::opt("partial", &BOOL),
    Field::opt("target", &STR),
]);

/// `sources/refs.rs` HTTP/git-host syntax refusals; other invalid refs carry `{}`.
pub(crate) const REF_INVALID: DetailSpec = DetailSpec::Object(&[Field::opt("host", &STR)]);

/// `state/desired.rs::invalid_at`, `app/sync.rs` and `app/export.rs`.
/// Item aggregation preserves this code's whole details object.
pub(crate) const DESIRED_STATE_INVALID: DetailSpec = DetailSpec::Object(&[
    Field::opt("first_line", &INT),
    Field::opt("key", &STR),
    Field::opt("line", &INT),
    Field::opt("other_line", &INT),
    Field::req("path", &STR),
    Field::req(
        "reason",
        &DetailSpec::Enum(&[
            "bad_value",
            "unknown_family",
            "newer_schema",
            "duplicate",
            "unknown_target_section",
            "empty",
            "edited",
        ]),
    ),
]);

/// `discover/registry.rs` selectors and missing clients; install/backup live selection.
pub(crate) const TARGET_NOT_FOUND: DetailSpec = DetailSpec::Object(&[
    Field::opt("candidates", &STRINGS),
    Field::opt("key", &STR),
    Field::opt("missing", &BOOL),
    Field::opt("mode", &STR),
    Field::opt("path", &STR),
    Field::opt("product", &STR),
    Field::opt("selector", &STR),
]);

/// `discover/registry.rs::one`, install/remove/backup/adopt selection and copy sides.
pub(crate) const TARGET_AMBIGUOUS: DetailSpec = DetailSpec::Object(&[
    Field::req("candidates", &STRINGS),
    Field::opt("flag", &STR),
    Field::opt("folder", &STR),
    Field::opt("selector", &STR),
]);

/// `sources/refs.rs::ambiguous`, backup prefix, remove, adoption and addon restore.
pub(crate) const REF_AMBIGUOUS: DetailSpec = DetailSpec::Object(&[
    Field::opt("addon", &STR),
    Field::opt(
        "candidates",
        &DetailSpec::Array(&DetailSpec::OneOf(&[
            &STR,
            &DetailSpec::Object(&[Field::req("from", &STR), Field::req("ref", &STR)]),
        ])),
    ),
    Field::opt("folder", &STR),
    Field::opt("input", &STR),
    Field::opt(
        "owned_now",
        &DetailSpec::Array(&DetailSpec::Object(&[
            Field::req("folder", &STR),
            Field::req("owner", &STR),
        ])),
    ),
    Field::opt("prefix", &STR),
    Field::opt("refs", &STRINGS),
    Field::opt("target", &STR),
]);

/// Backup, history/undo, argument resolution, sync, source and Omarchy lookups.
pub(crate) const NOT_FOUND: DetailSpec = DetailSpec::Object(&[
    Field::opt("addon", &STR),
    Field::opt("addons", &STRINGS),
    Field::opt("asset", &STR),
    Field::opt("assets", &STRINGS),
    Field::opt("backup", &STR),
    Field::opt("candidates", &STRINGS),
    Field::opt("family", &STR),
    Field::opt("id", &STR),
    Field::opt("name", &STR),
    Field::opt("newest", &STRINGS),
    Field::opt("owner", &STR),
    Field::opt("path", &STR),
    Field::opt("prefix", &STR),
    Field::opt(
        "reason",
        &DetailSpec::Enum(&["snapshot_pruned", "omarchy_missing", "widget_not_installed"]),
    ),
    Field::opt("ref", &STR),
    Field::opt("repository", &STR),
    Field::opt("rev", &STR),
    Field::opt("target", &STR).nullable(),
    Field::opt("tx", &STR),
    Field::opt("version", &STR),
]);

/// `app/remove.rs::not_managed` and detail-free name refusals.
pub(crate) const NOT_MANAGED: DetailSpec = DetailSpec::Object(&[
    Field::opt("class", &FOLDER_CLASSES),
    Field::opt("folder", &STR),
    Field::opt("target", &STR),
]);

/// `state/plans.rs::load` (every failure, including missing and oversized files:
/// `{plan_id, path, reason}`), `apply`'s lookup of an id or prefix (`{prefix}`) and its
/// reload of a plan removed while it waited (`{plan_id, path}`).
pub(crate) const PLAN_NOT_FOUND: DetailSpec = DetailSpec::Object(&[
    Field::opt("path", &STR),
    Field::opt("plan_id", &STR),
    Field::opt("prefix", &STR),
    Field::opt("reason", &DetailSpec::Enum(&["unreadable"])),
]);

/// `app/install.rs` client probing and `engine/plan.rs` candidate exhaustion.
pub(crate) const NO_COMPATIBLE_RELEASE: DetailSpec = DetailSpec::Object(&[
    Field::opt("checked", &STRINGS),
    Field::opt("ref", &STR),
    Field::opt("target", &STR),
    Field::opt("tried", &TRIED),
]);

/// `engine/running.rs::check`, its unknown-running wrapper and journal game waits.
pub(crate) const GAME_RUNNING: DetailSpec = DetailSpec::Object(&[
    Field::opt(
        "ops",
        &DetailSpec::Array(&DetailSpec::Object(&[
            Field::opt("files", &INT),
            Field::opt("folder", &STR),
            Field::req(
                "op",
                &DetailSpec::Enum(&["add", "replace", "remove", "restore_saved_variables"]),
            ),
        ])),
    ),
    Field::opt(
        "reason",
        &DetailSpec::Enum(&[
            "loaded_toc",
            "legacy_client",
            "policy_refuse",
            "saved_variables",
            // The undo of a repair, which never moves a running client's files (E7).
            "repair",
        ]),
    ),
    // A stuck journal's fence that refused the write (E14).
    Field::opt("resource", &STR),
    Field::opt("running", &DetailSpec::Enum(&["unknown"])),
    Field::opt("target", &STR),
    Field::opt("tx", &STR),
]);

/// `engine/ownership.rs::refused`; fsx and adoption may refuse without evidence.
pub(crate) const PROTECTED_FOLDER: DetailSpec =
    DetailSpec::Object(&[Field::opt("folders", &DetailSpec::Array(&REFUSED_FOLDER))]);

/// `engine/ownership.rs`, `engine/plan.rs::takeover_refused` and adoption conflicts.
pub(crate) const OWNERSHIP_CONFLICT: DetailSpec = DetailSpec::Object(&[
    Field::opt("folder", &STR),
    Field::opt("folders", &DetailSpec::Array(&REFUSED_FOLDER)),
    Field::opt("owner", &STR),
    Field::opt("owner_target", &STR),
    Field::opt("reason", &DetailSpec::Enum(&["owned_on_shared_target"])),
    Field::opt("within_transaction", &BOOL),
]);

/// `app/adopt/apply.rs::not_addon` and `engine/ownership.rs::refused`.
pub(crate) const UNMANAGED_FOLDER: DetailSpec = DetailSpec::Object(&[
    Field::opt(
        "class",
        &DetailSpec::Enum(&["linked", "invalid", "unmanaged"]),
    ),
    Field::opt("folder", &STR),
    Field::opt("folders", &DetailSpec::Array(&REFUSED_FOLDER)),
]);

/// `engine/plan.rs::one_addons_dir` and ownership's case-folded folder refusals.
pub(crate) const CASE_COLLISION: DetailSpec = DetailSpec::Object(&[
    Field::opt("folders", &DetailSpec::Array(&REFUSED_FOLDER)),
    Field::opt("paths", &STRINGS),
]);

/// `engine/plan.rs`, adoption's unloaded folders and backup family mismatches.
pub(crate) const WRONG_FLAVOR: DetailSpec = DetailSpec::Object(&[
    Field::opt("family", &STR),
    Field::opt("ref", &STR),
    Field::opt("target", &STR),
    Field::opt("target_family", &STR),
    Field::opt("tried", &TRIED),
    Field::opt("unloaded_folders", &STRINGS),
]);

/// `engine/inspect.rs` hostile checks and `backup/import.rs::refused`.
/// Aggregated item failures can omit the direct emitter's reason.
pub(crate) const UNSAFE_ARCHIVE: DetailSpec = DetailSpec::Object(&[
    Field::opt("compressed", &INT),
    Field::opt("distinct", &INT),
    Field::opt("entry", &STR),
    Field::opt("limit", &INT),
    Field::opt("measured", &INT),
    Field::opt("path", &STR).nullable(),
    Field::opt(
        "reason",
        &DetailSpec::Enum(&[
            "control_character",
            "absolute_path",
            "drive_letter",
            "parent_dir",
            "symlink",
            "special_file",
            "file_and_directory",
            "duplicate_entry",
            "case_collision",
            "not_utf8",
            "size_overrun",
            "invalid_folder_name",
            "ratio",
            "unsupported_entry",
            "bad_name",
            "duplicate_name",
            "duplicate_metadata",
            "missing_metadata",
            "manifest_mismatch",
            "symlink_escape",
        ]),
    ),
]);

/// Inspection, network bounds, git checkout, backup/import/tree and live folder indexing.
pub(crate) const ARCHIVE_TOO_LARGE: DetailSpec = DetailSpec::Object(&[
    Field::opt("config_key", &STR),
    Field::opt("constant", &STR),
    Field::opt("folder", &STR),
    Field::opt("limit", &INT),
    Field::opt("measured", &INT),
    Field::opt("reason", &DetailSpec::Enum(&["limit_exceeded"])),
]);

/// `engine/undo.rs` overlap, unreadable history, already-undone and restored-folder checks,
/// and `engine/repair/undo.rs`'s strict checks of a repair's undo (M4-D46): the paths a
/// predicate or a later change refused, and the guard kinds a set-aside's manifest found
/// changed, with the target they are about.
pub(crate) const UNDO_CONFLICT: DetailSpec = DetailSpec::Object(&[
    Field::opt(
        "changed_guards",
        &DetailSpec::Array(&DetailSpec::Enum(&["epochs", "generation", "marker"])),
    ),
    Field::opt(
        "conflicting",
        &DetailSpec::Array(&DetailSpec::Object(&[
            Field::req("folders", &STRINGS),
            Field::req("kind", &STR),
            Field::req("tx", &STR),
        ])),
    ),
    Field::opt("folder", &STR),
    Field::opt("lines", &INT),
    Field::opt("paths", &STRINGS),
    Field::opt(
        "reason",
        &DetailSpec::Enum(&[
            "history_unreadable",
            "changed_since_repair",
            "changed_since_suspension",
        ]),
    ),
    Field::opt("target", &STR),
    Field::req("tx", &STR),
    Field::opt("undone_by", &STR),
]);

/// `net/guard.rs`, `sources/url.rs` and `sources/git_hosts.rs` host guards.
pub(crate) const HOST_BLOCKED: DetailSpec = DetailSpec::Object(&[
    Field::opt("hop", &INT),
    Field::opt("host", &STR),
    Field::opt("url", &STR),
]);

/// fsx scope refusals, restore topology, and journal marker mismatch; a repair journal's
/// recovery stops name no single target (`target: null`).
pub(crate) const SCOPE_DENIED: DetailSpec = DetailSpec::Object(&[
    Field::opt("backup_dir", &STR),
    Field::opt(
        "reason",
        &DetailSpec::Enum(&["wtf_symlinked", "marker_mismatch", "skill_dir"]),
    ),
    // A stuck journal's fence that refused the write (E14).
    Field::opt("resource", &STR),
    Field::opt("target", &STR).nullable(),
    Field::opt("tx", &STR),
]);

/// `state/store.rs::too_new` and journal unrecoverable/unattributed failures.
pub(crate) const STATE_TOO_NEW: DetailSpec = DetailSpec::Object(&[
    Field::opt("path", &STR),
    Field::opt(
        "reason",
        &DetailSpec::Enum(&["journal_too_new", "unattributed"]),
    ),
    // A stuck journal's fence that refused the write (E14).
    Field::opt("resource", &STR),
    Field::opt("schema_version", &STR),
    Field::opt("target", &STR).nullable(),
    Field::opt("tx", &STR),
]);

/// `engine/restore.rs::unsupported` is the sole constructor.
pub(crate) const FILESYSTEM_UNSUPPORTED: DetailSpec = DetailSpec::Object(&[
    Field::req("backup_dir", &STR),
    Field::req(
        "reason",
        &DetailSpec::Enum(&[
            "wtf_is_mount_point",
            "stage_cross_mount",
            "no_rename_exchange",
        ]),
    ),
]);

/// `backup/export.rs::file_exists` and the equivalent dry-run check in `app/backup.rs`.
pub(crate) const FILE_EXISTS: DetailSpec = DetailSpec::Object(&[Field::req("file", &STR)]);

/// `fsx/lock.rs::busy`, and `apply`'s plan lease (`lease`, the plan id); item
/// aggregation can discard its holder evidence.
pub(crate) const LOCKED: DetailSpec = DetailSpec::Object(&[
    Field::opt("holder", &HOLDER).nullable(),
    Field::opt("lease", &STR),
]);

/// `app/errors.rs` incomplete/new error checks; other checks carry no details.
pub(crate) const CHECK_FAILED: DetailSpec = DetailSpec::Object(&[
    Field::opt("reason", &DetailSpec::Enum(&["incomplete", "new_errors"])),
    Field::opt("target", &STR),
]);

/// Network transport/decoding constructors carry no own details.
pub(crate) const NETWORK: DetailSpec = DetailSpec::EMPTY;
/// `Error::rate_limited`; item aggregation retains reset_at.
pub(crate) const RATE_LIMITED: DetailSpec = DetailSpec::Object(&[Field::req("reset_at", &STR)]);
/// Git subprocess download failures carry no own details.
pub(crate) const DOWNLOAD_FAILED: DetailSpec = DetailSpec::EMPTY;
/// `Error::http_status`; item aggregation can discard status.
pub(crate) const HTTP_STATUS: DetailSpec = DetailSpec::Object(&[Field::opt("status", &INT)]);

/// `engine/blobs.rs` expected/actual, a damaged cached archive's `blob` (with its
/// `actual`), backup verification's id, and candidate attempts.
pub(crate) const HASH_MISMATCH: DetailSpec = DetailSpec::Object(&[
    Field::opt("actual", &STR),
    Field::opt("backup", &STR),
    Field::opt("blob", &STR),
    Field::opt("expected", &STR),
    Field::opt("tried", &TRIED),
]);

/// Bound previews (`app/precondition.rs`), re-planning (`engine/tx.rs`) and `apply`'s
/// refusal of a plan that can no longer apply (`reason`, with `txs` or `path`); `{}`
/// elsewhere.
pub(crate) const PLAN_STALE: DetailSpec = DetailSpec::Object(&[
    Field::opt("cause", &STR),
    Field::opt("cause_message", &STR),
    Field::opt(
        "changed",
        &DetailSpec::Array(&DetailSpec::Enum(&[
            "command",
            "targets",
            "selection",
            "observed",
        ])),
    ),
    Field::opt("path", &STR),
    Field::opt(
        "reason",
        &DetailSpec::Enum(&[
            "already_applied",
            "expired",
            "binary_changed",
            "input_missing",
        ]),
    ),
    Field::opt("txs", &STRINGS),
]);

/// `engine/plan.rs::offline_archive`; other offline refusals have no own details.
pub(crate) const OFFLINE: DetailSpec = DetailSpec::Object(&[
    Field::opt("reason", &DetailSpec::Enum(&["not_cached"])),
    Field::opt("ref", &STR),
]);

const FILE_MEASUREMENT: DetailSpec =
    DetailSpec::Object(&[Field::req("sha256", &STR), Field::req("size", &INT)]);

/// Inspection, local source, stage extraction, backup import and WowAce listing/package.
/// `stage.rs` adds expected/actual measurements on extraction disagreement.
pub(crate) const BAD_ARCHIVE: DetailSpec = DetailSpec::Object(&[
    Field::opt("actual", &FILE_MEASUREMENT),
    Field::opt("expected", &FILE_MEASUREMENT),
    Field::opt("path", &STR).nullable(),
    Field::opt(
        "reason",
        &DetailSpec::Enum(&[
            "not_a_zip",
            "encrypted",
            "unsupported_compression",
            "corrupt",
            "no_addon_root",
            "duplicate_folder",
            "changed",
            "decoder_window",
            "truncated",
            "metadata_unreadable",
            "packaging_unsupported",
            "tree_collision",
            "not_a_listing",
            "duplicate_name",
            "bad_name",
        ]),
    ),
    Field::opt("unsupported", &STRINGS),
]);

/// Reserved, with no construction site.
pub(crate) const DISTRIBUTION_DISABLED: DetailSpec = DetailSpec::EMPTY;
/// Disabled sources and unsupported git invocations carry no own details.
pub(crate) const SOURCE_DISABLED: DetailSpec = DetailSpec::EMPTY;
/// Credential configuration refusals carry no own details.
pub(crate) const AUTH_REQUIRED: DetailSpec = DetailSpec::EMPTY;
/// Credential command/service failures carry no own details.
pub(crate) const AUTH_REJECTED: DetailSpec = DetailSpec::EMPTY;

/// Config/store parsing, backup/restore metadata, undo snapshots and journal stops.
pub(crate) const STATE_CORRUPT: DetailSpec = DetailSpec::Object(&[
    Field::opt("backup", &STR),
    Field::opt("column", &INT),
    Field::opt("key", &STR),
    Field::opt("line", &INT),
    Field::opt("path", &STR),
    Field::opt(
        "reason",
        &DetailSpec::Enum(&[
            "snapshot_mismatch",
            "journal_unreadable",
            "preservation_incomplete",
            "unattributed",
        ]),
    ),
    // A stuck journal's fence that refused the write (E14).
    Field::opt("resource", &STR),
    Field::opt("target", &STR).nullable(),
    Field::opt("tx", &STR),
]);

/// `Error::io`, backup tree/import/export, live indexing, git home and unattributed I/O.
pub(crate) const IO: DetailSpec = DetailSpec::Object(&[
    Field::opt("backup", &STR),
    Field::opt("folder", &STR),
    Field::opt("path", &STR).nullable(),
    Field::opt(
        "reason",
        &DetailSpec::Enum(&["unindexable", "unattributed"]),
    ),
    // A stuck journal's fence that refused the write (E14): its journal and target
    // (`null` for a repair, or a journal directory that cannot be listed).
    Field::opt("resource", &STR),
    Field::opt("target", &STR).nullable(),
    Field::opt("tx", &STR),
]);

/// `doctor --only` naming only online checks: the failure modes that need `--online`.
pub(crate) const ONLINE_REQUIRED: DetailSpec = DetailSpec::Object(&[Field::req("fms", &STRINGS)]);

/// Every refused doctor run (M4-D40): why, the failure mode or resources concerned, the
/// two fixes of a `fixer_conflict`, and for `over_budget` and `too_many_actions` what was
/// compared.
pub(crate) const DOCTOR_REFUSED: DetailSpec = DetailSpec::Object(&[
    Field::opt("actions", &INT),
    Field::opt("budget_ms", &INT),
    Field::opt("estimated_ms", &INT),
    Field::opt(
        "fixes",
        &DetailSpec::Array(&DetailSpec::Object(&[
            Field::req("fm", &STR),
            Field::req("instance", &STR),
        ])),
    ),
    Field::opt("fm", &STR),
    Field::opt("max_actions", &INT),
    Field::req(
        "reason",
        &DetailSpec::Enum(&[
            "force_required",
            "fixer_conflict",
            "too_many_actions",
            "over_budget",
            "filesystem_mismatch",
            "client_running",
            "not_enabled",
            "edited_locally",
        ]),
    ),
    Field::opt("resources", &STRINGS),
]);

/// Omarchy plugin validation and journal unexpected/unrecognized states (a repair's name
/// no single target: `target: null`); other bugs `{}`.
pub(crate) const INTERNAL: DetailSpec = DetailSpec::Object(&[
    Field::opt("problems", &STRINGS),
    Field::opt(
        "reason",
        &DetailSpec::Enum(&["plugin_invalid", "unexpected_state", "step_unrecognized"]),
    ),
    // A stuck journal's fence that refused the write (E14).
    Field::opt("resource", &STR),
    Field::opt("target", &STR).nullable(),
    Field::opt("tx", &STR),
]);

pub(super) fn details_schema(code: ErrorCode) -> Value {
    let mut schema = COMMON.to_json_schema();
    let own = code.detail_spec().to_json_schema();
    if let Some(properties) = own.get("properties").and_then(Value::as_object)
        && let Some(shared) = schema.get_mut("properties").and_then(Value::as_object_mut)
    {
        shared.extend(properties.clone());
    }
    let required: BTreeSet<String> = [schema.get("required"), own.get("required")]
        .into_iter()
        .flatten()
        .filter_map(Value::as_array)
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect();
    if !required.is_empty() {
        schema["required"] = Value::Array(required.into_iter().map(Value::String).collect());
    }
    schema
}

pub(super) fn check_details(code: ErrorCode, details: &Value) -> Result<(), Mismatch> {
    let Some(object) = details.as_object() else {
        return code.detail_spec().admits(details);
    };
    let mut own = Map::new();
    let mut shared = Map::new();
    for (key, value) in object {
        if code.detail_spec().field(key).is_some() {
            own.insert(key.clone(), value.clone());
        } else if COMMON.field(key).is_some() {
            shared.insert(key.clone(), value.clone());
        } else {
            return Err(Mismatch {
                path: key.clone(),
                expected: "a declared key".to_owned(),
                found: value.to_string(),
            });
        }
    }
    code.detail_spec().admits(&Value::Object(own))?;
    COMMON.admits(&Value::Object(shared))
}

pub(super) fn declared_detail_paths(code: ErrorCode) -> Vec<String> {
    code.detail_spec()
        .declared_paths()
        .into_iter()
        .chain(COMMON.declared_paths())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::path::Path;

    use crate::app::precondition::{self, Part};
    use crate::engine::inspect::HostileReason;
    use crate::error::{Error, Suggestion};
    use crate::inventory::FolderClass;

    #[test]
    fn every_code_has_a_well_formed_object_spec() {
        assert_eq!(COMMON.well_formed(), Ok(()), "COMMON: {COMMON:?}");
        super::super::detail_spec::check_rendered(&COMMON.to_json_schema()).unwrap();
        for code in ErrorCode::ALL {
            let spec = code.detail_spec();
            assert!(matches!(spec, DetailSpec::Object(_)), "{code}: {spec:?}");
            assert_eq!(spec.well_formed(), Ok(()), "{code}: {spec:?}");
            let schema = code.details_schema();
            assert_eq!(
                super::super::detail_spec::check_rendered(&schema),
                Ok(()),
                "{code}: {schema}"
            );
        }
    }

    #[test]
    fn error_details_schemas() {
        let schemas: BTreeMap<_, _> = ErrorCode::ALL
            .iter()
            .map(|code| (code.wire(), code.details_schema()))
            .collect();
        insta::with_settings!({snapshot_path => "../snapshots"}, {
            insta::assert_json_snapshot!(schemas);
        });
    }

    #[test]
    fn own_keys_never_contradict_common() {
        let DetailSpec::Object(shared) = COMMON else {
            unreachable!()
        };
        for code in ErrorCode::ALL {
            let rendered = code.details_schema();
            let shared_schema = COMMON.to_json_schema();
            for field in shared {
                if let Some(own) = code.detail_spec().field(field.key) {
                    assert_eq!(own, field, "{code}: conflicting shared key {}", field.key);
                }
                assert_eq!(
                    rendered["properties"][field.key], shared_schema["properties"][field.key],
                    "{code}: shared schema {}",
                    field.key
                );
            }
        }
    }

    #[test]
    fn declared_detail_paths_include_common() {
        for code in ErrorCode::ALL {
            let paths = code.declared_detail_paths();
            for path in [
                "items",
                "items[].code",
                "items[].error.details",
                "required_by",
                "journal_pending",
                "recovery_error.code",
            ] {
                assert!(
                    paths.iter().any(|got| got == path),
                    "{code}: missing {path:?} in {paths:?}"
                );
            }
            assert_eq!(
                paths.iter().collect::<BTreeSet<_>>().len(),
                paths.len(),
                "{code}: unique paths"
            );
            assert!(
                paths.windows(2).all(|pair| pair[0] < pair[1]),
                "{code}: canonical order {paths:?}"
            );
        }
        assert!(
            ErrorCode::PlanStale
                .declared_detail_paths()
                .contains(&"changed".to_owned())
        );
        assert_eq!(COMMON.opaque_paths(), ["items[].error.details"]);
    }

    #[test]
    fn changed_is_the_precondition_parts() {
        let values = PLAN_STALE
            .field("changed")
            .unwrap()
            .spec
            .enum_values()
            .unwrap();
        assert_eq!(
            values,
            Part::ALL.map(Part::wire),
            "changed must name the token parts"
        );
    }

    #[test]
    fn unsafe_archive_reasons_cover_hostile_reasons() {
        let values = UNSAFE_ARCHIVE
            .field("reason")
            .unwrap()
            .spec
            .enum_values()
            .unwrap();
        for reason in HostileReason::ALL {
            assert!(
                values.contains(&reason.wire()),
                "{reason:?}: missing from {values:?}"
            );
        }
    }

    #[test]
    fn folder_classes_are_the_inventory_classes() {
        // An exhaustive match makes a newly added class require an explicit review.
        fn wire(class: FolderClass) -> &'static str {
            match class {
                FolderClass::Protected => "protected",
                FolderClass::Managed => "managed",
                FolderClass::Shared => "shared",
                FolderClass::Linked => "linked",
                FolderClass::Unmanaged => "unmanaged",
                FolderClass::Invalid => "invalid",
            }
        }
        let classes = [
            FolderClass::Protected,
            FolderClass::Managed,
            FolderClass::Shared,
            FolderClass::Linked,
            FolderClass::Unmanaged,
            FolderClass::Invalid,
        ];
        let expected = classes.map(wire);
        for class in classes {
            assert_eq!(
                serde_json::to_value(class).unwrap(),
                wire(class),
                "{class:?}: serde spelling"
            );
        }
        for spec in [&REFUSED_FOLDER, &NOT_MANAGED] {
            assert_eq!(
                spec.field("class").unwrap().spec.enum_values().unwrap(),
                expected,
                "{spec:?}"
            );
        }
        for value in UNMANAGED_FOLDER
            .field("class")
            .unwrap()
            .spec
            .enum_values()
            .unwrap()
        {
            assert!(expected.contains(value), "unmanaged subset {value:?}");
        }
    }

    #[test]
    fn helper_constructors_match_their_specs() {
        let fix = || Suggestion::text("retry the operation");
        let mut cases = vec![
            (
                "io".to_owned(),
                Error::io(Path::new("x/AddOns"), &std::io::Error::other("test"), fix()),
            ),
            (
                "http_status_503".to_owned(),
                Error::http_status(503, "unavailable", fix()),
            ),
            (
                "http_status_404".to_owned(),
                Error::http_status(404, "missing", fix()),
            ),
            (
                "rate_limited".to_owned(),
                Error::rate_limited("2026-10-27T12:00:00Z".parse().unwrap(), "limited", fix()),
            ),
        ];
        let argv = vec![
            "omaddons".to_owned(),
            "install".to_owned(),
            "--dry-run".to_owned(),
        ];
        for part in Part::ALL {
            cases.push((
                format!("stale_{}", part.wire()),
                precondition::stale("install", &[part], &argv),
            ));
        }
        cases.push((
            "stale_all_parts".to_owned(),
            precondition::stale("install", &Part::ALL, &argv),
        ));
        for (name, error) in cases {
            assert_eq!(
                error.code.check_details(&error.details),
                Ok(()),
                "{name}: {error:?}"
            );
        }
    }

    #[test]
    fn check_details_rejects_a_retyped_changed() {
        for (name, details, path) in [
            (
                "retyped",
                serde_json::json!({"changed":"observed"}),
                "changed",
            ),
            (
                "undeclared_value",
                serde_json::json!({"changed":["moved"]}),
                "changed[0]",
            ),
            ("undeclared_key", serde_json::json!({"extra":1}), "extra"),
        ] {
            let error = ErrorCode::PlanStale.check_details(&details).unwrap_err();
            assert_eq!(error.path, path, "{name}: {details}: {error:?}");
            if name == "undeclared_value" {
                assert!(
                    error.expected.contains("observed"),
                    "{name}: closed set in {error:?}"
                );
            }
        }
        let error = ErrorCode::PlanStale
            .check_details(&Value::Null)
            .unwrap_err();
        assert!(error.path.is_empty());
        assert_eq!(error.expected, "object");
    }

    fn one_of_paths(spec: &DetailSpec, path: &str, paths: &mut BTreeSet<String>) {
        match spec {
            DetailSpec::Object(fields) => {
                for field in *fields {
                    let next = if path.is_empty() {
                        field.key.to_owned()
                    } else {
                        format!("{path}.{}", field.key)
                    };
                    one_of_paths(field.spec, &next, paths);
                }
            }
            DetailSpec::Array(item) => one_of_paths(item, &format!("{path}[]"), paths),
            DetailSpec::OneOf(branches) => {
                // A union of array elements is reported under that array's key.
                paths.insert(path.strip_suffix("[]").unwrap_or(path).to_owned());
                for branch in *branches {
                    one_of_paths(branch, path, paths);
                }
            }
            _ => {}
        }
    }

    #[test]
    fn two_shape_keys_are_the_known_ones() {
        let mut got = BTreeSet::new();
        for (name, spec) in ErrorCode::ALL
            .iter()
            .map(|code| (code.wire(), code.detail_spec()))
            .chain([("COMMON", &COMMON)])
        {
            let mut paths = BTreeSet::new();
            one_of_paths(spec, "", &mut paths);
            got.extend(paths.into_iter().map(|path| (name.to_owned(), path)));
        }
        let expected: BTreeSet<_> = [
            ("ref_ambiguous", "candidates"),
            ("case_collision", "folders[].existing"),
            ("ownership_conflict", "folders[].existing"),
            ("protected_folder", "folders[].existing"),
            ("unmanaged_folder", "folders[].existing"),
            ("COMMON", "items"),
        ]
        .map(|(code, path)| (code.to_owned(), path.to_owned()))
        .into_iter()
        .collect();
        assert_eq!(
            got, expected,
            "a new two-shape key needs a reason in its leaf's close comment; prefer one shape"
        );
    }

    #[test]
    fn clipped_item_errors_and_minimal_conflicts_match() {
        use crate::app::outcome::ItemError;
        for (code, details) in [
            (ErrorCode::Locked, serde_json::json!({"holder":null})),
            (ErrorCode::HttpStatus, serde_json::json!({"status":503})),
            (
                ErrorCode::UnsafeArchive,
                serde_json::json!({"reason":"symlink","entry":"Foo/x"}),
            ),
            (ErrorCode::ArchiveTooLarge, serde_json::json!({"limit":1})),
        ] {
            let original = Error::new(code, "test", Suggestion::text("retry")).details(details);
            let clipped = ItemError::from(&original).error();
            assert_eq!(
                code.check_details(&clipped.details),
                Ok(()),
                "{code}: clipped {clipped:?}"
            );
        }
        let minimal = serde_json::json!({"within_transaction":true,"folders":[{"folder":"Foo","owner":"other"}]});
        assert_eq!(
            ErrorCode::OwnershipConflict.check_details(&minimal),
            Ok(()),
            "{minimal}"
        );
        let extraction = serde_json::json!({"reason":"changed","path":"Foo/Foo.lua","expected":{"size":1,"sha256":"a"},"actual":{"size":2,"sha256":"b"}});
        assert_eq!(
            ErrorCode::BadArchive.check_details(&extraction),
            Ok(()),
            "{extraction}"
        );
    }

    #[test]
    fn shared_keys_are_checked_with_the_same_strictness() {
        let details = serde_json::json!({
            "items":[{"error":null,"request":"Foo","target":"retail"}],
            "journal_pending":"tx", "required_by":["Foo"],
            "recovery_error":{"code":"io","message":"failed"},
        });
        for code in [ErrorCode::Network, ErrorCode::PlanStale] {
            assert_eq!(code.check_details(&details), Ok(()), "{code}: {details}");
            let invalid =
                serde_json::json!({"recovery_error":{"code":"io","message":"failed","extra":1}});
            assert_eq!(
                code.check_details(&invalid).unwrap_err().path,
                "recovery_error.extra"
            );
        }
    }
}
