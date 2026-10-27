//! Output fields whose values may change across otherwise identical invocations.

pub const VOLATILE: &[(&str, &str)] = &[
    ("meta.*", "the envelope's volatile block"),
    ("data.**.duration_ms", "timings"),
    ("data.**.lock_held_ms", "timings"),
    ("data.**.plan_id", "a new plan per previewed change"),
    ("data.**.plan_expires_at", "a new plan per previewed change"),
];
