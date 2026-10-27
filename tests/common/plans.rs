//! Stored-plan fixtures and volatile-id redaction, shared by the plan contract and
//! end-to-end suites. Copies start from a real plan so its typed intent stays valid.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use jiff::{SignedDuration, Timestamp};
use serde_json::Value;
use ulid::Ulid;

use super::env::TestEnv;

/// Replaces each 26-character Crockford run at a word boundary with `<ulid>`.
pub fn redact_ulids(text: &str) -> String {
    let crockford = |c: char| c.is_ascii_digit() || (c.is_ascii_uppercase() && !"ILOU".contains(c));
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let run = chars[i..].iter().take_while(|c| crockford(**c)).count();
        let boundary = i == 0 || !chars[i - 1].is_ascii_alphanumeric();
        if run == 26 && boundary {
            out.push_str("<ulid>");
            i += 26;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

/// The plan JSON files, excluding the store lock and per-plan leases, in path order.
pub fn plan_files(env: &TestEnv) -> Vec<PathBuf> {
    let dir = env.cache().join("omaddons/plans");
    let mut paths: Vec<PathBuf> = match fs::read_dir(&dir) {
        Ok(entries) => entries
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
            .collect(),
        Err(error) => {
            assert_eq!(
                error.kind(),
                std::io::ErrorKind::NotFound,
                "list plans in {}: {error}",
                dir.display()
            );
            Vec::new()
        }
    };
    paths.sort();
    paths
}

/// Writes a copy at `id`, dated `created_at`, with the ordinary 24-hour lifetime.
/// Everything else in the real plan is preserved, and the copy remains owner-only.
pub fn age_plan(env: &TestEnv, template: &Value, id: Ulid, created_at: Timestamp) {
    let mut plan = template.clone();
    plan["plan_id"] = Value::from(id.to_string());
    plan["created_at"] = Value::from(created_at.to_string());
    plan["expires_at"] = Value::from(
        created_at
            .checked_add(SignedDuration::from_hours(24))
            .unwrap()
            .to_string(),
    );
    let dir = env.cache().join("omaddons/plans");
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{id}.json"));
    fs::write(&path, serde_json::to_vec(&plan).unwrap()).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
}

/// Plants `n` live copies of `template`, each with `padding` extra summary bytes.
/// Time comes from the real plan so a test with `OMADDONS_NOW` remains deterministic.
pub fn plant_live(env: &TestEnv, template: &Value, n: usize, padding: usize) {
    let created_at = template["created_at"].as_str().unwrap().parse().unwrap();
    let mut plan = template.clone();
    plan["summary"]
        .as_array_mut()
        .unwrap()
        .push(Value::from("x".repeat(padding)));
    for _ in 0..n {
        age_plan(env, &plan, Ulid::generate(), created_at);
    }
}
