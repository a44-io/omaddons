//! Output fields whose values may change across otherwise identical invocations.

pub const VOLATILE: &[(&str, &str)] = &[
    ("meta.*", "the envelope's volatile block"),
    ("data.**.duration_ms", "timings"),
    ("data.**.lock_held_ms", "timings"),
    ("data.**.plan_id", "a new plan per previewed change"),
    ("data.**.plan_expires_at", "a new plan per previewed change"),
    // A doctor envelope carries its report's keys at the top level (M4-D34).
    ("run_id", "a new doctor run per invocation"),
    ("run_dir", "a new doctor run per invocation"),
    ("started_at", "a new doctor run per invocation"),
    ("finished_at", "a new doctor run per invocation"),
    ("summary.duration_ms", "timings"),
    // `doctor fix`'s real runs. A dry run writes its own names `<run>` and `<tx>`, so
    // its planned actions are no new values.
    ("tx", "a new transaction"),
    ("actions_jsonl_path", "a new doctor run per invocation"),
    ("backups_dir", "a new doctor run per invocation"),
    ("undo_command", "a new doctor run per invocation"),
    ("undo_argv", "a new doctor run per invocation"),
    ("actions.*.backup", "a new doctor run per invocation"),
];
