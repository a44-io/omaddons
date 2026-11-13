//! Bounds, expiry and hostile inputs for the plan blob pin reader.
#![cfg(test)]

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};

use super::test_support::{paths, permission_skip, plan_with, plant};
use super::*;

fn now() -> Timestamp {
    "2026-10-28T12:00:00Z".parse().unwrap()
}
fn at(seconds: i64) -> Timestamp {
    now()
        .checked_add(SignedDuration::from_secs(seconds))
        .unwrap()
}
fn hash(c: char) -> String {
    c.to_string().repeat(64)
}

#[test]
fn pins_are_every_blob_of_every_live_plan() {
    let (_dir, paths) = paths();
    let (a, b, c) = (hash('a'), hash('b'), hash('c'));
    for (created, blobs) in [
        (at(-3), vec![Some(a.as_str()), Some(b.as_str())]),
        (at(-2), vec![Some(b.as_str()), Some(c.as_str())]),
        (at(-1), vec![None]),
    ] {
        plant(&paths, &plan_with(created, &blobs));
    }
    let pins = unexpired_blob_pins(&paths, now());
    assert_eq!(pins.blobs, BTreeSet::from([a, b, c]), "{pins:?}");
    assert_eq!(
        (pins.live, pins.read, pins.complete),
        (3, 3, true),
        "{pins:?}"
    );
}

#[test]
fn pins_follow_expiry() {
    for (case, created, counts) in [
        ("expired_long_ago", at(-2 * 86400), (0, 1, 0)),
        ("expires_now", at(-86400), (0, 1, 0)),
        ("one_second_left", at(-86400 + 1), (1, 0, 0)),
        ("full_ttl", now(), (1, 0, 0)),
        ("dated_ahead", at(1), (0, 0, 1)),
        ("far_future", at(30 * 86400), (0, 0, 1)),
    ] {
        let (_dir, paths) = paths();
        plant(&paths, &plan_with(created, &[Some(&hash('a'))]));
        let pins = unexpired_blob_pins(&paths, now());
        assert_eq!(
            (pins.live, pins.expired, pins.future),
            counts,
            "{case}: created {created}: {pins:?}"
        );
        assert_eq!(pins.blobs.len(), counts.0, "{case}: {pins:?}");
    }
}

#[test]
fn unreadable_plans_pin_nothing() {
    for case in [
        "truncated",
        "not_json",
        "empty_file",
        "token_edited",
        "display_argv_edited",
        "newer_version",
        "id_not_the_file_name",
        "oversized",
        "mode_000",
        "symlink_named_like_a_plan",
        "directory_named_like_a_plan",
    ] {
        let (dir, paths) = paths();
        let control = plan_with(at(-1), &[Some(&hash('c'))]);
        plant(&paths, &control);
        let bad = plan_with(now(), &[Some(&hash('b'))]);
        let path = plant(&paths, &bad);
        let bytes = fs::read(&path).unwrap();
        let mut value: Value = serde_json::from_slice(&bytes).unwrap();
        match case {
            "truncated" => fs::write(&path, &bytes[..bytes.len() / 2]).unwrap(),
            "not_json" => fs::write(&path, b"not json").unwrap(),
            "empty_file" => fs::write(&path, b"").unwrap(),
            "oversized" => fs::write(
                &path,
                vec![b' '; usize::try_from(PLAN_MAX_BYTES + 1).unwrap()],
            )
            .unwrap(),
            "mode_000" => {
                fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
                if fs::read(&path).is_ok() {
                    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
                    permission_skip("unreadable_plans_pin_nothing/mode_000");
                    continue;
                }
            }
            "symlink_named_like_a_plan" => {
                let outside = dir.path().join("outside.json");
                fs::rename(&path, &outside).unwrap();
                symlink(outside, &path).unwrap();
            }
            "directory_named_like_a_plan" => {
                fs::remove_file(&path).unwrap();
                fs::create_dir(&path).unwrap();
            }
            _ => {
                match case {
                    "token_edited" => value["precondition"]["token"] = Value::from("pc1:bad"),
                    "display_argv_edited" => value["display_argv"] = json!(["omaddons", "status"]),
                    "newer_version" => value["schema_version"] = Value::from("1.3"),
                    "id_not_the_file_name" => {
                        value["plan_id"] = Value::from(control.plan_id.to_string())
                    }
                    _ => {}
                }
                fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
            }
        }
        let pins = unexpired_blob_pins(&paths, now());
        if case == "mode_000" {
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        assert_eq!(
            pins.blobs,
            BTreeSet::from([hash('c')]),
            "{case}: {bad:?}: {pins:?}"
        );
        let ignored = matches!(
            case,
            "symlink_named_like_a_plan" | "directory_named_like_a_plan"
        );
        assert_eq!(pins.unreadable, usize::from(!ignored), "{case}: {pins:?}");
        assert_eq!(
            pins.read,
            if ignored || case == "oversized" { 1 } else { 2 },
            "{case}: {pins:?}"
        );
    }
}

#[test]
fn malformed_blob_values_are_not_pinned() {
    for (case, blob) in [
        ("uppercase_hex", "A".repeat(64)),
        ("63_digits", "a".repeat(63)),
        ("65_digits", "a".repeat(65)),
        ("non_hex", "g".repeat(64)),
        ("empty", String::new()),
    ] {
        let (_dir, paths) = paths();
        let plan = plan_with(now(), &[Some(&blob)]);
        plant(&paths, &plan);
        let pins = unexpired_blob_pins(&paths, now());
        assert!(pins.blobs.is_empty(), "{case}: {plan:?}: {pins:?}");
        assert_eq!(pins.live, 1, "{case}: {pins:?}");
        let mixed = plan_with(at(-1), &[Some(&hash('c')), Some(&blob)]);
        plant(&paths, &mixed);
        assert_eq!(
            unexpired_blob_pins(&paths, now()).blobs,
            BTreeSet::from([hash('c')]),
            "{case}: mixed {mixed:?}"
        );
    }
}

#[test]
fn a_missing_store_pins_nothing_and_is_complete() {
    let (_dir, paths) = paths();
    assert_eq!(
        unexpired_blob_pins(&paths, now()),
        PlanPins {
            complete: true,
            ..PlanPins::default()
        }
    );
    assert!(!paths.plans_dir().exists());
}

#[test]
fn an_unlistable_store_pins_nothing_and_is_incomplete() {
    let (_dir, paths) = paths();
    fs::create_dir_all(paths.plans_dir()).unwrap();
    fs::set_permissions(paths.plans_dir(), fs::Permissions::from_mode(0o000)).unwrap();
    let bypassed = fs::read_dir(paths.plans_dir()).is_ok();
    let pins = unexpired_blob_pins(&paths, now());
    fs::set_permissions(paths.plans_dir(), fs::Permissions::from_mode(0o700)).unwrap();
    if bypassed {
        permission_skip("an_unlistable_store_pins_nothing_and_is_incomplete");
        return;
    }
    assert_eq!(pins, PlanPins::default(), "{pins:?}");
}

#[test]
fn the_file_and_byte_bounds_keep_the_newest_plans() {
    let (_dir, paths) = paths();
    let mut files = Vec::new();
    for (i, c) in ['a', 'b', 'c'].into_iter().enumerate() {
        files.push(plant(
            &paths,
            &plan_with(at(i64::try_from(i).unwrap() - 3), &[Some(&hash(c))]),
        ));
    }
    let sizes: Vec<_> = files
        .iter()
        .map(|p| fs::metadata(p).unwrap().len())
        .collect();
    for (case, max_files, max_bytes, expected, complete) in [
        ("file_bound", 2, u64::MAX, vec![hash('b'), hash('c')], false),
        (
            "file_boundary",
            3,
            u64::MAX,
            vec![hash('a'), hash('b'), hash('c')],
            true,
        ),
        (
            "byte_bound",
            usize::MAX,
            sizes[1] + sizes[2],
            vec![hash('b'), hash('c')],
            false,
        ),
        (
            "byte_boundary",
            usize::MAX,
            sizes.iter().sum(),
            vec![hash('a'), hash('b'), hash('c')],
            true,
        ),
    ] {
        let pins = pins_within(&paths, now(), max_files, max_bytes);
        assert_eq!(
            pins.read,
            expected.len(),
            "{case}: sizes {sizes:?}: {pins:?}"
        );
        assert_eq!(
            pins.blobs,
            expected.into_iter().collect(),
            "{case}: {pins:?}"
        );
        assert_eq!(pins.complete, complete, "{case}: {pins:?}");
    }
}

#[test]
fn unexpired_blob_pins_uses_the_store_quotas() {
    let (_dir, paths) = paths();
    for i in 0..PLANS_MAX_FILES + PLANS_SWEEP_MAX + 1 {
        let created = now()
            .checked_sub(SignedDuration::from_millis(i64::try_from(i).unwrap()))
            .unwrap();
        plant(&paths, &plan_with(created, &[Some(&format!("{i:064x}"))]));
    }
    let pins = unexpired_blob_pins(&paths, now());
    assert_eq!(
        (pins.read, pins.blobs.len(), pins.complete),
        (PLANS_MAX_FILES, PLANS_MAX_FILES, false),
        "{pins:?}"
    );
    assert!(pins.bytes <= PLANS_MAX_BYTES, "{pins:?}");
}

#[test]
fn pin_discovery_writes_nothing() {
    let (dir, paths) = paths();
    plant(&paths, &plan_with(now(), &[Some(&hash('a'))]));
    plant(&paths, &plan_with(at(-2 * 86400), &[Some(&hash('b'))]));
    let bad = plant(&paths, &plan_with(at(-1), &[Some(&hash('c'))]));
    fs::write(&bad, b"bad json").unwrap();
    let listing = |path: &Path| {
        let mut rows: Vec<_> = fs::read_dir(path)
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                let meta = entry.metadata().unwrap();
                (
                    entry.file_name(),
                    meta.len(),
                    meta.modified().unwrap(),
                    meta.permissions().mode(),
                )
            })
            .collect();
        rows.sort();
        rows
    };
    let before = listing(&paths.plans_dir());
    let parent = listing(dir.path());
    let pins = unexpired_blob_pins(&paths, now());
    assert_eq!(listing(&paths.plans_dir()), before, "{pins:?}");
    assert_eq!(listing(dir.path()), parent, "{pins:?}");
}
