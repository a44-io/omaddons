//! The bar widget's contract with the CLI (M3-D2, M3-D41): `omarchy/Model.js`'s
//! template table and path registry against the binary's own, the widget constants
//! against `politeness.rs` and `limits.rs`, and the two pipeline steps around bun's
//! tests. `write_widget_fixtures` writes real envelopes for bun's reducers;
//! `validate_built_argv` parses exactly the argv bun built. Both are `#[ignore]`: the
//! widget pipeline (`scripts/gate.sh`, `argc widget-test`) runs them in order, with a
//! fresh `$OMADDONS_WIDGET_OUT`.
//! e2e suite: the widget's contract with the CLI, and the plan runner's scenes (M4-D28) as the real binary answers them.

use crate::common;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;

use common::home::Home;
use common::http::{gh_feed, gh_latest, gh_latest_status, gh_release, github_search};
use common::tlog::{TestLog, tlog};
use common::tukui::{elvui, tukui_list};
use common::wowi::{FileEntry, wowi_filelist};
use omaddons::app::intent::StoredIntent;
use omaddons::app::precondition::BINDING_COMMANDS;
use omaddons::app::{self, command_name};
use omaddons::cli::Cli;
use serde_json::Value;
use support::{
    DESTRUCTIVE, Rel, behind, cli_paths, constant, fill, leading_path, model_js, out_dir, parse,
    positional_and_option_values, strings, templates, widget_flags, write_fixture,
};

/// `CLI_PATHS` has one row per registry path, with the registry's `mutates`, the
/// destructive paths and the binding paths (A4's `BINDING_COMMANDS`).
#[test]
fn cli_paths_mirror_the_registry() {
    let rows = cli_paths();
    let registry: BTreeSet<&str> = app::COMMANDS.iter().map(|c| c.name).collect();
    let listed: BTreeSet<&str> = rows.keys().map(String::as_str).collect();
    assert_eq!(listed, registry, "CLI_PATHS rows differ from app::COMMANDS");
    for (path, row) in &rows {
        assert_eq!(row["mutates"], app::mutates(path), "{path}: mutates");
        assert_eq!(
            row["destructive"],
            DESTRUCTIVE.contains(&path.as_str()),
            "{path}: destructive"
        );
        assert_eq!(
            row["binds"],
            BINDING_COMMANDS.contains(&path.as_str()),
            "{path}: binds"
        );
    }
    for path in DESTRUCTIVE.iter().chain(BINDING_COMMANDS) {
        assert!(registry.contains(path), "{path} is no registry path");
    }
}

/// `PLAN_PATHS` lists exactly the registry paths whose dry runs store a plan (M4-D22),
/// each once: the paths whose previewed actions the widget confirms through their plan.
#[test]
fn plan_paths_mirror_the_registry() {
    let listed: Vec<String> = serde_json::from_value(support::block(&model_js(), "PLAN_PATHS"))
        .expect("PLAN_PATHS is a list of strings");
    let unique: BTreeSet<&str> = listed.iter().map(String::as_str).collect();
    assert_eq!(
        unique.len(),
        listed.len(),
        "PLAN_PATHS repeats a path: {listed:?}"
    );
    let storing: BTreeSet<&str> = app::COMMANDS
        .iter()
        .filter(|c| c.stores_plan)
        .map(|c| c.name)
        .collect();
    assert_eq!(
        unique, storing,
        "PLAN_PATHS differs from app::COMMANDS' stores_plan"
    );
    let registry: BTreeSet<&str> = app::COMMANDS.iter().map(|c| c.name).collect();
    for path in &unique {
        assert!(registry.contains(path), "{path} is no registry path");
    }
}

/// README's key table, between `<!-- BEGIN KEYMAP` and `<!-- END KEYMAP -->` in the
/// `omarchy` section, is `KEYMAP`'s rows in order (design doc §3): each row's keys, the
/// title of its context (`KEY_CONTEXTS`) and its label. One table drives the widget's
/// dispatcher, its key reference and its tooltips, and this keeps README in step.
#[test]
fn readme_key_table_is_the_keymap() {
    let source = model_js();
    let rows = support::block(&source, "KEYMAP");
    let contexts = support::block(&source, "KEY_CONTEXTS");
    let titles: BTreeMap<&str, &str> = contexts
        .as_array()
        .expect("KEY_CONTEXTS is a list")
        .iter()
        .map(|c| (c["id"].as_str().unwrap(), c["title"].as_str().unwrap()))
        .collect();
    let mut expected = vec![
        "| Keys | Where | Does |".to_owned(),
        "|---|---|---|".to_owned(),
    ];
    for row in rows.as_array().expect("KEYMAP is a list") {
        let keys: Vec<String> = row["keys"]
            .as_array()
            .unwrap()
            .iter()
            .map(|k| format!("`{}`", k.as_str().unwrap()))
            .collect();
        let context = row["context"].as_str().unwrap();
        let title = titles.get(context);
        assert!(
            title.is_some(),
            "KEYMAP context {context} has no KEY_CONTEXTS title"
        );
        expected.push(format!(
            "| {} | {} | {} |",
            keys.join(" "),
            title.copied().unwrap_or_default(),
            row["label"].as_str().unwrap()
        ));
    }
    let readme =
        fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("README.md"))
            .expect("README.md reads");
    let begin = readme.find("<!-- BEGIN KEYMAP");
    let end = readme.find("<!-- END KEYMAP -->");
    assert!(
        begin.is_some() && end > begin,
        "README.md has no <!-- BEGIN KEYMAP --> … <!-- END KEYMAP --> table"
    );
    let (begin, end) = (begin.unwrap_or_default(), end.unwrap_or_default());
    let table: Vec<&str> = readme[begin..end]
        .lines()
        .skip(1)
        .filter(|l| !l.trim().is_empty())
        .collect();
    assert_eq!(
        table,
        expected,
        "README's key table differs from KEYMAP in omarchy/Model.js; it should read:\n{}",
        expected.join("\n")
    );
}

/// The plan scenarios (`omarchy/tests/qml/tst_plans.qml`, over the Fixtures.js the
/// pipeline generates) read only fixtures step 1 writes, and keep their thirteen
/// scenarios; the slice-2 demo asserts the first three by name.
#[test]
fn the_plan_scenarios_read_only_fixtures_the_binary_writes() {
    const SCENARIOS: &[&str] = &[
        "test_a_routine_matching_update_applies_with_no_dialog",
        "test_a_matching_update_that_adds_a_dependency_opens_the_dialog_naming_it",
        "test_a_moved_release_opens_the_dialog_with_changed_since_you_looked",
        "test_a_matching_update_that_replaces_local_edits_opens_the_dialog",
        "test_a_matching_update_with_extra_removals_opens_the_dialog",
        "test_equal_versions_from_another_asset_open_the_dialog",
        "test_a_stale_apply_previews_again_and_never_reruns_apply",
        "test_an_expired_plan_previews_again",
        "test_a_missing_plan_previews_again",
        "test_a_no_op_preview_refreshes_and_runs_nothing",
        "test_update_all_applies_when_every_row_matches",
        "test_a_removal_confirms_and_runs_apply_of_its_preview",
        "test_undo_last_confirms_through_apply",
    ];
    let qml = fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("omarchy/tests/qml/tst_plans.qml"),
    )
    .unwrap();
    // Every quoted name that is a plan fixture's (the file holds no escaped quote).
    let named: BTreeSet<&str> = qml
        .split('"')
        .skip(1)
        .step_by(2)
        .filter(|s| s.starts_with("plan-"))
        .collect();
    assert!(!named.is_empty(), "tst_plans.qml names no plan fixture");
    for name in &named {
        assert!(
            support::PLAN_FIXTURES.contains(name),
            "tst_plans.qml reads {name}, which plans_fixtures does not write"
        );
    }
    for scenario in SCENARIOS {
        assert!(
            qml.contains(&format!("function {scenario}()")),
            "tst_plans.qml has no {scenario}"
        );
    }
}

/// The repair scenarios (`omarchy/tests/qml/tst_repairs.qml`, over the generated
/// Fixtures.js) read only fixtures `repair_fixtures` writes, and keep their six scenarios.
#[test]
fn the_repair_scenarios_read_only_fixtures_the_binary_writes() {
    const SCENARIOS: &[&str] = &[
        "test_a_credential_fix_from_issues_is_undone_with_undo_last_through_a_stored_plan",
        "test_a_set_aside_row_offers_no_fix_and_shows_its_explanation_and_command",
        "test_undo_last_reverts_a_repair_through_its_stored_plan",
        "test_a_credential_fix_previews_then_runs_bound_to_its_token",
        "test_a_stale_credential_fix_previews_again_and_runs_nothing_twice",
        "test_a_credential_fix_finished_after_the_panel_closed_notifies_once",
    ];
    let qml = fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("omarchy/tests/qml/tst_repairs.qml"),
    )
    .unwrap();
    // Every quoted name that is a repair fixture's (the file holds no escaped quote).
    let named: BTreeSet<&str> = qml
        .split('"')
        .skip(1)
        .step_by(2)
        .filter(|s| s.starts_with("repair-") || s.starts_with("setaside-"))
        .collect();
    assert!(!named.is_empty(), "tst_repairs.qml names no repair fixture");
    for name in &named {
        assert!(
            support::REPAIR_FIXTURES.contains(name),
            "tst_repairs.qml reads {name}, which repair_fixtures does not write"
        );
    }
    for scenario in SCENARIOS {
        assert!(
            qml.contains(&format!("function {scenario}()")),
            "tst_repairs.qml has no {scenario}"
        );
    }
}

/// Stored previews use the same destructive policy as the widget's confirmation
/// dialogs; the widget's other destructive path (`targets rm`) stores no plan.
#[test]
fn stored_intents_are_destructive_where_the_widget_confirms() {
    let rows = cli_paths();
    for (path, destructive) in StoredIntent::PATHS {
        assert_eq!(
            destructive,
            DESTRUCTIVE.contains(&path),
            "{path}: stored intent and confirmation policy differ"
        );
        assert!(rows.contains_key(path), "{path}: no widget CLI_PATHS row");
        let row = &rows[path];
        assert_eq!(
            row["destructive"], destructive,
            "{path}: stored intent and widget CLI_PATHS disagree: {row}"
        );
    }
}

/// Every template is a registry command whose `mutates` is the registry's, previews
/// what is destructive (and asks first, `preview: confirm`), previews only on a path that
/// stores a plan (M4-D28), sets no flag the widget owns, and keeps every slot one argv
/// element where a hostile value cannot become a flag (M3-D2).
#[test]
fn every_template_is_a_registry_command_with_safe_slots() {
    let owned = widget_flags();
    let plan_paths: Vec<String> =
        serde_json::from_value(support::block(&model_js(), "PLAN_PATHS")).unwrap();
    for (name, template) in templates() {
        let argv = strings(&template["argv"]);
        let path = leading_path(&argv);
        assert!(path.is_some(), "{name}: no command path");
        let path = path.unwrap_or_default();
        let mutates = template["mutates"] == true;
        assert_eq!(mutates, app::mutates(&path), "{name}: mutates");
        assert!(
            template.get("confirm").is_none(),
            "{name}: the confirm key is gone; a preview: confirm template asks first"
        );
        let preview = template.get("preview");
        if let Some(preview) = preview {
            assert!(
                preview == "match" || preview == "confirm" || preview == "bind",
                "{name}: preview is match, confirm or bind, not {preview}"
            );
            if preview == "bind" {
                // G2: a bound preview's Yes is its own argv with the preview's token, on a
                // path that binds and stores no plan (M4-D44), never a destructive one.
                assert!(
                    BINDING_COMMANDS.contains(&path.as_str()),
                    "{name}: bind on {path}, which takes no --if-match"
                );
                assert!(
                    !plan_paths.contains(&path),
                    "{name}: bind on {path}, a path that stores plans"
                );
                assert!(
                    !DESTRUCTIVE.contains(&path.as_str()),
                    "{name}: bind on the destructive {path}"
                );
            } else {
                assert!(
                    plan_paths.contains(&path),
                    "{name}: a preview on {path}, which stores no plan"
                );
            }
        }
        // Consent is a terminal's (M4-D38), and gc and doctor undo are never the widget's.
        assert!(
            path != "doctor gc" && path != "doctor undo",
            "{name}: a template on {path}"
        );
        if DESTRUCTIVE.contains(&path.as_str()) {
            assert!(
                preview.is_some_and(|p| p == "confirm"),
                "{name}: {path} needs preview: confirm"
            );
        }
        if mutates {
            assert!(
                template.get("lane").is_none(),
                "{name}: a mutation has no lane"
            );
        } else {
            assert!(
                template["lane"] == "local" || template["lane"] == "network",
                "{name}: lane"
            );
        }
        let separator = argv.iter().position(|a| a == "--");
        for (i, part) in argv.iter().enumerate() {
            let after = separator.is_some_and(|s| i > s);
            let flag = part.split('=').next().unwrap_or(part);
            if part.starts_with('-') && Some(i) != separator {
                assert!(!after, "{name}: flag {part} after --");
                assert!(
                    !owned.contains(&flag.to_owned()) && flag != "--dry-run",
                    "{name}: {flag} is the widget's"
                );
            }
            assert!(
                flag != "--force" && flag != "--yes",
                "{name}: {flag} gives consent, which only a terminal gives"
            );
            let Some(open) = part.find('{') else { continue };
            let slot = &part[open + 1..part.len() - 1];
            assert!(part.ends_with('}'), "{name}: malformed slot {part}");
            assert!(
                ["target", "addon", "ref", "backup", "query", "fm", "path"].contains(&slot),
                "{name}: unknown slot {part}"
            );
            // A client folder the player types (1n2p.32) is the one positional of
            // `targets add`, after --, never anything else's.
            if slot == "path" {
                assert!(
                    part == "{path}" && path == "targets add" && after,
                    "{name}: {{path}} outside targets add's positional ({part} on {path})"
                );
            }
            // A failure-mode id is one `--only=` element of `doctor fix`, never a list.
            if slot == "fm" {
                assert!(
                    part == "--only={fm}" && path == "doctor fix",
                    "{name}: {{fm}} outside --only on doctor fix ({part} on {path})"
                );
            }
            if slot == "query" {
                assert!(!mutates, "{name}: free text in a mutation");
            }
            if open == 0 {
                let target_value =
                    slot == "target" && i > 0 && (argv[i - 1] == "-t" || argv[i - 1] == "--target");
                assert!(after || target_value, "{name}: positional {part} before --");
            } else {
                assert!(
                    part.starts_with("--") && part[..open].ends_with('=') && !after,
                    "{name}: {part} is not --opt={{slot}} before --"
                );
            }
        }
    }
}

/// `omarchy/manifest.json` is exactly the planned manifest (F1, M3-D5, M3-D10); E1 stamps
/// its `version` at install.
#[test]
fn the_manifest_is_the_planned_one() {
    let manifest: Value = serde_json::from_str(
        &fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("omarchy/manifest.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let expected = serde_json::json!({
      "schemaVersion": 1,
      "id": "a44.omaddons",
      "name": "omaddons",
      "version": "0.0.0-unstamped",
      "author": "a44",
      "description": "World of Warcraft addons on every client: updates, Lua errors and WTF backups, from the omaddons CLI.",
      "kinds": ["service", "bar-widget"],
      "entryPoints": { "service": "Service.qml", "barWidget": "BarWidget.qml" },
      "barWidget": {
        "displayName": "omaddons",
        "description": "One gamepad for every WoW client: a badge for pending addon updates, urgent when something is broken or an addon threw Lua errors, and a keyboard panel to update, undo, back up and restore. Every action is an omaddons command line, recorded and undoable.",
        "category": "Games",
        "aliases": ["omaddons", "wow", "warcraft", "addons"],
        "allowMultiple": false,
        "defaultSection": "right",
        "defaults": { "binary": "", "panelWidth": 540, "autoCheck": "On", "notifyUpdates": "Off", "notifyErrors": "On", "refreshIntervalSec": 300, "keyHint": "On" },
        "schema": [
          { "key": "binary", "type": "path", "label": "omaddons binary", "defaultValue": "", "description": "Empty runs the binary that installed the widget (omaddons omarchy install stamps its path). Set a path to run another build." },
          { "key": "panelWidth", "type": "integer", "label": "Panel width", "min": 400, "max": 800, "step": 20, "defaultValue": 540 },
          { "key": "autoCheck", "type": "enum", "label": "Check for updates when due", "options": ["On", "Off"], "defaultValue": "On", "description": "Run omaddons outdated in the background when a client's last check is older than check_interval_hours in omaddons' config.toml." },
          { "key": "notifyUpdates", "type": "enum", "label": "Notify about new updates", "options": ["On", "Off"], "defaultValue": "Off" },
          { "key": "notifyErrors", "type": "enum", "label": "Notify about Lua errors after a session", "options": ["On", "Off"], "defaultValue": "On" },
          { "key": "refreshIntervalSec", "type": "integer", "label": "Fallback refresh (seconds)", "min": 30, "max": 3600, "step": 30, "defaultValue": 300, "description": "Safety-net refresh behind the doorbell; 15 s while the panel is open." },
          { "key": "keyHint", "type": "enum", "label": "Show the keybinding tip", "options": ["On", "Off"], "defaultValue": "On", "description": "After the panel is first opened by mouse, a card shows the line that binds a key to it. Seeing it turns this Off." }
        ]
      }
    });
    assert_eq!(manifest, expected);
}

/// The widget's id is a literal in manifest.json only; every other use goes through
/// `Build.PLUGIN_ID`, so E1's debug id seam yields a separate widget (F7 check 8). It
/// scans every QML and JS file under `omarchy/`, tests included, and must find a planted
/// `serviceFor("<id>")`.
#[test]
fn the_plugin_id_is_a_literal_in_the_manifest_only() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("omarchy");
    let manifest: Value =
        serde_json::from_str(&fs::read_to_string(root.join("manifest.json")).unwrap()).unwrap();
    let id = manifest["id"].as_str().unwrap();
    let mut files = Vec::new();
    let mut stack = vec![root.clone()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "qml" || e == "js") {
                let name = path.strip_prefix(&root).unwrap().display().to_string();
                files.push((name, fs::read_to_string(&path).unwrap()));
            }
        }
    }
    assert!(!files.is_empty(), "no QML or JS under omarchy/");
    assert_eq!(id_literals(&files, id), Vec::<String>::new());

    let bar = files
        .iter_mut()
        .find(|(name, _)| name == "BarWidget.qml")
        .unwrap();
    assert!(bar.1.contains("serviceFor(Build.PLUGIN_ID)"));
    bar.1 = bar.1.replacen(
        "serviceFor(Build.PLUGIN_ID)",
        &format!("serviceFor(\"{id}\")"),
        1,
    );
    let found = id_literals(&files, id);
    assert!(
        found.len() == 1 && found[0].starts_with("BarWidget.qml:"),
        "{found:?}"
    );
}

/// The widget lists `status`'s issues generically and matches no failure-mode id by name
/// (M4-D48), so an id the CLI narrows or splits changes nothing it decides. It scans every
/// production QML and JS file under `omarchy/` (its tests aside) for `fm-` before a
/// lowercase letter and for each id of the doctor's registry, and must find an id planted
/// in a copy of `Model.js`.
#[test]
fn the_widget_matches_no_issue_id_by_name() {
    use omaddons::doctor::registry::FAILURE_MODES;

    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("omarchy");
    let mut files = Vec::new();
    let mut stack = vec![root.clone()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let name = path.strip_prefix(&root).unwrap().display().to_string();
            if path.is_dir() {
                if name != "tests" {
                    stack.push(path);
                }
            } else if path.extension().is_some_and(|e| e == "qml" || e == "js") {
                files.push((name, fs::read_to_string(&path).unwrap()));
            }
        }
    }
    assert!(
        files.iter().any(|(name, _)| name == "Model.js"),
        "no Model.js under omarchy/"
    );
    // `name:line` of every `fm-` followed by a lowercase ASCII letter.
    let ids_named = |files: &[(String, String)]| -> Vec<String> {
        let mut out = Vec::new();
        for (name, text) in files {
            for (at, _) in text.match_indices("fm-") {
                let next = text[at + 3..].chars().next();
                if next.is_some_and(|c| c.is_ascii_lowercase()) {
                    out.push(format!("{name}:{}", text[..at].matches('\n').count() + 1));
                }
            }
        }
        out
    };
    assert_eq!(ids_named(&files), Vec::<String>::new());
    for row in FAILURE_MODES {
        assert_eq!(
            id_literals(&files, row.id),
            Vec::<String>::new(),
            "{}",
            row.id
        );
    }

    let planted = "fm-concurrency_primitives-journal-pending";
    let model = files
        .iter_mut()
        .find(|(name, _)| name == "Model.js")
        .unwrap();
    let line = model.1.lines().count() + 1;
    model
        .1
        .push_str(&format!("const planted = \"{planted}\";\n"));
    let at = format!("Model.js:{line}");
    assert_eq!(ids_named(&files), std::slice::from_ref(&at));
    assert_eq!(id_literals(&files, planted), [at]);
}

/// `name:line` of every file that names `id`. An id that merely starts with it
/// (`a44.omaddons-fixture`) is another id.
#[cfg(test)]
fn id_literals(files: &[(String, String)], id: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (name, text) in files {
        for (at, _) in text.match_indices(id) {
            let next = text[at + id.len()..].chars().next();
            if !next.is_some_and(|c| c.is_alphanumeric() || c == '-' || c == '_') {
                out.push(format!("{name}:{}", text[..at].matches('\n').count() + 1));
            }
        }
    }
    out
}

/// The embedded asset table is exactly the runtime files under `omarchy/`, its tests and
/// a generated `Build.js` aside, so a view added there is embedded in the same commit
/// (E1). A row naming a missing file already fails the build, through `include_str!`.
#[test]
fn every_runtime_file_is_embedded() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("omarchy");
    let mut on_disk = BTreeSet::new();
    let mut pending = vec![root.clone()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let relative = path
                .strip_prefix(&root)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            if relative == "tests" || relative == "Build.js" {
                continue;
            }
            if path.is_dir() {
                pending.push(path);
            } else {
                on_disk.insert(relative);
            }
        }
    }
    let embedded: BTreeSet<String> = omaddons::omarchy::assets::ASSETS
        .iter()
        .map(|(path, _)| (*path).to_owned())
        .collect();
    assert_eq!(on_disk, embedded);
}

/// The flags the widget owns and puts first are the binary's global flags, so a clap
/// rename fails here and in `validate_built_argv`.
#[test]
fn the_widget_flags_are_global_flags_of_the_binary() {
    use clap::CommandFactory;
    let cli = Cli::command();
    let globals: BTreeSet<String> = cli
        .get_arguments()
        .filter(|a| a.is_global_set())
        .filter_map(|a| a.get_long().map(|l| format!("--{l}")))
        .collect();
    for flag in widget_flags()
        .iter()
        .map(String::as_str)
        .chain(["--dry-run"])
    {
        assert!(
            globals.contains(flag),
            "{flag} is no global flag of omaddons"
        );
    }
}

/// A network read's inactivity deadline outlasts the longest request politeness.rs
/// allows: every attempt timing out, and every retry waiting the longest Retry-After.
#[test]
fn the_network_deadline_outlasts_the_longest_polite_request() {
    use omaddons::politeness::{API_READ_TIMEOUT, MAX_RETRIES, RETRY_AFTER_CAP};
    let deadline = constant(&model_js(), "WIDGET_NETWORK_DEADLINE_SEC");
    let longest = u64::from(MAX_RETRIES + 1) * API_READ_TIMEOUT.as_secs()
        + u64::from(MAX_RETRIES) * RETRY_AFTER_CAP.as_secs();
    assert!(
        deadline >= longest,
        "WIDGET_NETWORK_DEADLINE_SEC {deadline} < {longest}"
    );
}

/// Every template's worst-case output stays under `WIDGET_OUTPUT_MAX_BYTES`. The worst
/// case is every Battle.net flavor plus two custom clients, 400 addons each, all with
/// an update. Templates that list every addon are measured at 10 and 30 addons and
/// extrapolated per addon; one-addon templates are bounded by one client's share at
/// one addon; `search` and the changelog by their limits.
#[test]
fn the_output_cap_bounds_every_template() {
    use omaddons::limits::{
        CHANGELOG_MAX_CHARS, CHANGELOG_MAX_ENTRIES, ERRORS_LOCALS_MAX_CHARS,
        ERRORS_MESSAGE_MAX_CHARS, ERRORS_SHOWN_PER_ADDON, ERRORS_STACK_LINES_VERBOSE,
        SEARCH_MAX_RESULTS, SEARCH_SUMMARY_MAX_CHARS,
    };
    // A raw stack frame's characters (a path and a function name).
    const FRAME: u64 = 512;
    // A template's `--limit` value.
    let limit_of = |template: &Value| -> u64 {
        let args = template["argv"].as_array().unwrap();
        let at = args.iter().position(|a| a == "--limit").unwrap();
        args[at + 1].as_str().unwrap().parse().unwrap()
    };
    const CLIENTS: u64 = 12;
    const ADDONS: u64 = 400;
    // Room per row for what the limits do not cap (names, refs, dates), and per answer.
    const ROW: u64 = 4096;
    const ANSWER: u64 = 64 * 1024;
    let cap = constant(&model_js(), "WIDGET_OUTPUT_MAX_BYTES");
    let t = templates();
    let measured = ["status", "status_full", "list", "check", "update_all"];
    let sizes = |n: usize| -> BTreeMap<&str, u64> {
        let home = behind(n);
        let mut out = BTreeMap::new();
        for name in measured {
            let run = home.exec(&fill(&t, name, &[]));
            run.expect(0);
            out.insert(name, run.stdout.len() as u64);
        }
        out
    };
    let (small, large) = (sizes(10), sizes(30));
    let mut per_client = BTreeMap::new();
    for name in measured {
        // These list every addon; a flat answer means the homes did not differ.
        if matches!(name, "list" | "check" | "update_all") {
            assert!(large[name] > small[name], "{name}: {small:?} {large:?}");
        }
        let slope = large[name].saturating_sub(small[name]).div_ceil(20);
        let base = small[name].saturating_sub(10 * slope);
        per_client.insert(name, (base, slope));
        let worst = CLIENTS * (base + ADDONS * slope);
        assert!(worst < cap, "{name}: worst case {worst} bytes ≥ {cap}");
    }
    // The previewed Update all (M4-D28) and the apply of its plan, each on homes of
    // its own so the measurements above are unaffected.
    let plan_sizes = |n: usize| -> (u64, u64) {
        let home = behind(n);
        let preview = home.exec(&support::preview_of(fill(&t, "update_all", &[])));
        let id = support::planned(&preview.expect(0));
        let apply = home.exec(&support::apply_of(&id));
        apply.expect(0);
        (preview.stdout.len() as u64, apply.stdout.len() as u64)
    };
    let (small_plan, large_plan) = (plan_sizes(10), plan_sizes(30));
    for (name, small, large) in [
        ("update_all_preview", small_plan.0, large_plan.0),
        ("apply", small_plan.1, large_plan.1),
    ] {
        assert!(large > small, "{name}: {small} {large}");
        let slope = (large - small).div_ceil(20);
        let base = small.saturating_sub(10 * slope);
        let worst = CLIENTS * (base + ADDONS * slope);
        assert!(worst < cap, "{name}: worst case {worst} bytes ≥ {cap}");
    }
    let (base, slope) = per_client["update_all"];
    for (name, template) in &t {
        let argv = template["argv"].to_string();
        let bound = if measured.contains(&name.as_str()) {
            continue;
        } else if name == "update_target" || name == "adopt_target" {
            // One client's preview: an item and an approval row per addon (a take-over
            // adopts at most every folder there, 1n2p.32).
            Some(base + ADDONS * slope)
        } else if argv.contains("{query}") {
            Some(SEARCH_MAX_RESULTS as u64 * (SEARCH_SUMMARY_MAX_CHARS as u64 * 6 + ROW) + ANSWER)
        } else if argv.contains("--catalog") {
            // `search --catalog` has no cap: the built-in catalog's 66 entries beside up
            // to ~900 of a player's own (`catalog.d`), each row a hint per client. A
            // catalog past that fails the read as oversized, and Search's empty box then
            // says what to type (1n2p.26).
            const CATALOG_ENTRIES: u64 = 1_000;
            Some(
                CATALOG_ENTRIES * (SEARCH_SUMMARY_MAX_CHARS as u64 * 6 + ROW + CLIENTS * 32)
                    + ANSWER,
            )
        } else if argv.contains("--changelog") {
            Some(CHANGELOG_MAX_ENTRIES as u64 * (CHANGELOG_MAX_CHARS as u64 * 6 + ROW) + ANSWER)
        } else if name == "errors" {
            // `--limit` addons per client, each listing at most ERRORS_SHOWN_PER_ADDON
            // strict errors.
            Some(CLIENTS * (limit_of(template) * ERRORS_SHOWN_PER_ADDON as u64 * ROW + ANSWER))
        } else if name == "errors_target" {
            // The same, for one client.
            Some(limit_of(template) * ERRORS_SHOWN_PER_ADDON as u64 * ROW + ANSWER)
        } else if name == "backups" {
            // `--limit` backups per client.
            Some(CLIENTS * (limit_of(template) * ROW + ANSWER))
        } else if name == "backups_detail" {
            // One client's `--limit` backups, each with the addons that wrote it.
            Some(limit_of(template) * (ROW + ADDONS * 256) + ANSWER)
        } else if matches!(
            name.as_str(),
            "backup_target" | "backup_all" | "backup_auto" | "restore" | "prune_backup"
        ) {
            // A record per client: ids and counts, never a list of files.
            Some(CLIENTS * ROW + ANSWER)
        } else if name == "targets_add" {
            // The clients the folder held (a Battle.net root holds several), one record
            // each (1n2p.32).
            Some(CLIENTS * ROW + ANSWER)
        } else if name == "errors_raw" {
            // One client, one addon, each error with its raw message, locals and the
            // verbose stack beside the strict one.
            let raw = (ERRORS_MESSAGE_MAX_CHARS + ERRORS_LOCALS_MAX_CHARS) as u64 * 6
                + 2 * ERRORS_STACK_LINES_VERBOSE as u64 * FRAME * 6;
            Some(ERRORS_SHOWN_PER_ADDON as u64 * (raw + ROW) + ANSWER)
        } else if name == "dismiss_errors" {
            // Counts per client, never a list of errors.
            Some(CLIENTS * ROW + ANSWER)
        } else if matches!(name.as_str(), "doctor_fix" | "doctor_fix_target") {
            // A repair's report (G2): at most DOCTOR_FIX_MAX_ACTIONS planned primitives
            // (a larger plan is refused before it is reported), and every client's
            // findings.
            Some(
                omaddons::limits::DOCTOR_FIX_MAX_ACTIONS as u64 * support::ACTION_ROW
                    + CLIENTS * ADDONS * support::FINDING_ROW
                    + ANSWER,
            )
        } else if argv.contains("{addon}") || argv.contains("{ref}") {
            Some(base + slope)
        } else {
            None
        };
        assert!(bound.is_some(), "{name}: bound its output in this test");
        assert!(
            bound.is_some_and(|b| b < cap),
            "{name}: worst case {bound:?} bytes ≥ {cap}"
        );
    }
    // The CLI reads the widget runs from CLI argv, not from a template (G2): a row's
    // `doctor explain`, every client's findings at worst.
    let explain = CLIENTS * ADDONS * support::FINDING_ROW + ANSWER;
    assert!(
        explain < cap,
        "doctor explain: worst case {explain} bytes ≥ {cap}"
    );
}

/// Pipeline step 3: every argv bun built (`built-argv.json`) parses with the binary's
/// clap as the command it was built for, with the widget's flags first and every
/// hostile slot value arriving whole as its positional or option value; every CLI argv
/// the classifier accepted parses as the path it reported, and every one it refused is
/// refused for the stated reason. Nothing is substituted here.
#[test]
#[ignore = "run by the widget pipeline after bun wrote built-argv.json"]
fn validate_built_argv() {
    let doc: Value =
        serde_json::from_str(&fs::read_to_string(out_dir().join("built-argv.json")).unwrap())
            .unwrap();
    let built = doc["built"].as_array().unwrap();
    assert!(!built.is_empty(), "bun built nothing");
    let names: BTreeSet<&str> = built
        .iter()
        .filter_map(|e| e["template"].as_str())
        .collect();
    let shipped = templates();
    assert_eq!(
        names,
        shipped.keys().map(String::as_str).collect::<BTreeSet<_>>(),
        "bun skipped a template"
    );
    for entry in built {
        let argv = strings(&entry["argv"]);
        let cli = parse(&argv);
        assert_eq!(argv[0], doc["binary"], "{argv:?}");
        assert_eq!(
            argv[1..5],
            ["--json", "--actor", "widget", "--reason"],
            "{argv:?}: the standard flags come first"
        );
        // Past `--` clap reads a flag as one more positional, and parsing still works:
        // only slot values may follow it.
        let slots: Vec<&str> = entry["slots"]
            .as_object()
            .unwrap()
            .values()
            .filter_map(Value::as_str)
            .collect();
        if let Some(separator) = argv.iter().position(|a| a == "--") {
            for part in &argv[separator + 1..] {
                assert!(
                    slots.contains(&part.as_str()),
                    "{argv:?}: {part:?} after -- is no slot value"
                );
            }
        }
        assert!(cli.global.json, "{argv:?}: --json");
        assert_eq!(cli.global.actor.as_deref(), Some("widget"), "{argv:?}");
        assert!(
            cli.global
                .reason
                .as_deref()
                .is_some_and(|r| r.ends_with(" (bar widget)")),
            "{argv:?}: --reason"
        );
        let path = command_name(cli.command.as_ref());
        assert_eq!(path, entry["path"], "{argv:?}");
        assert_eq!(entry["mutates"], app::mutates(path), "{argv:?}");
        assert_eq!(cli.global.dry_run, entry["preview"] == true, "{argv:?}");
        assert_eq!(
            cli.global.if_match.as_deref().unwrap_or(""),
            entry["token"].as_str().unwrap(),
            "{argv:?}"
        );
        let template = strings(&shipped[entry["template"].as_str().unwrap()]["argv"]);
        let values = positional_and_option_values(&argv);
        for (slot, value) in entry["slots"].as_object().unwrap() {
            let value = value.as_str().unwrap();
            if slot == "target" {
                assert_eq!(cli.global.target.as_deref(), Some(value), "{argv:?}");
                continue;
            }
            let element = template
                .iter()
                .find(|p| p.ends_with(&format!("{{{slot}}}")));
            let expected = match element.and_then(|p| p.strip_suffix(&format!("={{{slot}}}"))) {
                Some(option) => (option.trim_start_matches('-').to_owned(), value.to_owned()),
                None => (String::new(), value.to_owned()),
            };
            assert!(
                values.contains(&expected),
                "{argv:?}: {{{slot}}} = {value:?} did not arrive as {expected:?} ({values:?})"
            );
        }
    }

    let owned = widget_flags();
    for entry in doc["classified"].as_array().unwrap() {
        let argv = strings(&entry["argv"]);
        let verdict = &entry["verdict"];
        let end = argv.iter().position(|a| a == "--").unwrap_or(argv.len());
        if verdict["ok"] == true {
            let reported = verdict["path"].as_str().unwrap();
            assert_eq!(
                leading_path(&argv[1..]).as_deref(),
                Some(reported),
                "{argv:?}"
            );
            assert_eq!(verdict["mutates"], app::mutates(reported), "{argv:?}");
            assert_eq!(
                verdict["destructive"],
                DESTRUCTIVE.contains(&reported),
                "{argv:?}"
            );
            assert_eq!(
                verdict["binds"],
                BINDING_COMMANDS.contains(&reported),
                "{argv:?}"
            );
            let runnable = strings(&entry["runnable"]);
            let cli = parse(&runnable);
            let path = command_name(cli.command.as_ref());
            assert!(
                path == reported || (reported == "backup" && path == "backup list"),
                "{runnable:?} parses as {path}, not {reported}"
            );
            assert_eq!(cli.global.actor.as_deref(), Some("widget"), "{runnable:?}");
            if entry["refusal"] == "destructive_without_preview" {
                assert!(
                    DESTRUCTIVE.contains(&reported) && !argv[..end].contains(&"--dry-run".into()),
                    "{argv:?}"
                );
                assert!(cli.global.dry_run, "{runnable:?} runs as a preview");
            } else {
                assert_eq!(entry["refusal"], "", "{argv:?}");
                assert!(
                    !DESTRUCTIVE.contains(&reported) || cli.global.dry_run,
                    "{runnable:?}: a destructive run without its preview"
                );
            }
            continue;
        }
        let reason = verdict["reason"].as_str().unwrap_or_default();
        assert!(
            [
                "widget_flag",
                "unknown_path",
                "not_omaddons",
                "malformed",
                "apply_outside_plan",
                "irreversible",
                "consent_flag",
                "doctor_gc",
                "own_flow_only"
            ]
            .contains(&reason),
            "{argv:?}: unknown reason {reason:?}"
        );
        // G2: the first two words before a flag, as the classifier reads them.
        let words: Vec<&str> = argv[1..end]
            .iter()
            .take_while(|a| !a.is_empty() && !a.starts_with('-'))
            .take(2)
            .map(String::as_str)
            .collect();
        match reason {
            "consent_flag" => assert!(
                argv[1..]
                    .iter()
                    .any(|a| matches!(a.split('=').next(), Some("--force" | "--yes"))),
                "{argv:?}: consent_flag without --force or --yes"
            ),
            "doctor_gc" => assert_eq!(words, ["doctor", "gc"], "{argv:?}"),
            "own_flow_only" => assert!(
                words == ["doctor", "fix"] || words == ["doctor", "undo"],
                "{argv:?}: own_flow_only on {words:?}"
            ),
            "irreversible" => assert!(
                argv[1..end]
                    .iter()
                    .any(|a| a.split('=').next() == Some("--purge")),
                "{argv:?}: irreversible without --purge"
            ),
            "widget_flag" => assert!(
                argv[1..end]
                    .iter()
                    .any(|a| owned.contains(&a.split('=').next().unwrap_or(a).to_owned())),
                "{argv:?}"
            ),
            "unknown_path" => assert_eq!(leading_path(&argv[1..end]), None, "{argv:?}"),
            "not_omaddons" => assert_ne!(argv[0], "omaddons", "{argv:?}"),
            "apply_outside_plan" => assert_eq!(
                leading_path(&argv[1..end]).as_deref(),
                Some("apply"),
                "{argv:?}: apply_outside_plan refuses an apply only"
            ),
            _ => assert!(argv.len() < 2, "{argv:?}"),
        }
    }

    // The apply a preview's plan runs (M4-D28): the plan's id, as offered, with the
    // widget's flags and nothing else; never a preview, never bound.
    let planned = doc["planned"].as_array().unwrap();
    assert!(!planned.is_empty(), "bun built no plan's apply");
    for entry in planned {
        let argv = strings(&entry["argv"]);
        let cli = parse(&argv);
        assert_eq!(argv[0], doc["binary"], "{argv:?}");
        assert_eq!(
            command_name(cli.command.as_ref()),
            "apply",
            "{argv:?}: apply …"
        );
        assert!(!cli.global.dry_run, "{argv:?}: apply … is no preview");
        assert_eq!(
            cli.global.if_match, None,
            "{argv:?}: apply … carries its plan's token"
        );
        assert_eq!(cli.global.actor.as_deref(), Some("widget"), "{argv:?}");
        assert!(
            cli.global
                .reason
                .as_deref()
                .is_some_and(|r| r.ends_with(" (bar widget)")),
            "{argv:?}: --reason"
        );
        let plan_id = match cli.command {
            Some(omaddons::cli::Command::Apply(args)) => Some(args.plan_id),
            _ => None,
        };
        assert_eq!(
            plan_id.as_deref(),
            entry["plan_id"].as_str(),
            "{argv:?}: apply … runs its plan's id"
        );
    }
}

#[cfg(test)]
mod support {
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::{Path, PathBuf};

    use omaddons::cli::Cli;
    use serde_json::Value;

    use super::common::addons::toc;
    use super::common::home::{Home, Run};
    use super::common::http::{Asset, gh_latest, gh_release_v, paths};

    /// One planned repair primitive in a `doctor fix` report: two predicates, its
    /// paths, its backup and its size (G2). `repair_fixtures` measures the binary's
    /// rows against it: the credential chmod's is 352 bytes.
    pub const ACTION_ROW: u64 = 1024;
    /// One finding: its instance, label, evidence, remediation and explain argv. Measured
    /// by `repair_fixtures`: a `doctor fix` preview's finding is 922 bytes and a set-aside
    /// journal's `doctor explain` instance 1,378 (its journal path and attribution
    /// evidence), so 1,500; the doctor_fix bound of the_output_cap_bounds_every_template
    /// then stays below WIDGET_OUTPUT_MAX_BYTES (it allows up to about 1,520).
    pub const FINDING_ROW: u64 = 1500;

    /// The paths whose runs always go through a preview (M3-D9).
    pub const DESTRUCTIVE: &[&str] = &[
        "remove",
        "undo",
        "doctor undo",
        "doctor gc",
        "backup restore",
        "backup prune",
        "targets rm",
    ];

    /// `omarchy/Model.js`.
    pub fn model_js() -> String {
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("omarchy/Model.js")).unwrap()
    }

    /// The JSON literal of `var <name> = …` between `// BEGIN <name>` and `// END <name>`.
    pub fn block(source: &str, name: &str) -> Value {
        let begin = source.find(&format!("// BEGIN {name}"));
        let end = begin.and_then(|b| source[b..].find(&format!("// END {name}")).map(|e| e + b));
        let assignment = format!("var {name} = ");
        let start = begin.zip(end).and_then(|(b, e)| {
            source[b..e]
                .find(&assignment)
                .map(|s| s + b + assignment.len())
        });
        assert!(
            start.is_some(),
            "Model.js has no `{assignment}` between // BEGIN {name} and // END {name}"
        );
        let (start, end) = (start.unwrap_or_default(), end.unwrap_or_default());
        let parsed: Result<Value, _> = serde_json::from_str(source[start..end].trim());
        assert!(parsed.is_ok(), "{name} is not a JSON literal: {parsed:?}");
        parsed.unwrap_or_default()
    }

    /// The template table.
    pub fn templates() -> BTreeMap<String, Value> {
        serde_json::from_value(block(&model_js(), "COMMANDS")).unwrap()
    }

    /// The widget's path registry.
    pub fn cli_paths() -> BTreeMap<String, Value> {
        serde_json::from_value(block(&model_js(), "CLI_PATHS")).unwrap()
    }

    /// `var WIDGET_FLAGS = [...]`: the flags the widget owns.
    pub fn widget_flags() -> Vec<String> {
        let source = model_js();
        let line = source
            .lines()
            .find(|l| l.starts_with("var WIDGET_FLAGS = "))
            .expect("Model.js defines WIDGET_FLAGS on one line");
        serde_json::from_str(line.split_once(" = ").unwrap().1).unwrap()
    }

    pub fn strings(value: &Value) -> Vec<String> {
        value
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_owned())
            .collect()
    }

    /// The registry path an argument list (no program name) starts with: the longest
    /// one its first one or two words form before any flag. The classifier's rule,
    /// written again here so the two implementations can disagree.
    pub fn leading_path(args: &[String]) -> Option<String> {
        let words: Vec<&str> = args
            .iter()
            .take_while(|a| !a.is_empty() && !a.starts_with('-'))
            .take(2)
            .map(String::as_str)
            .collect();
        let known = |path: &str| omaddons::app::COMMANDS.iter().any(|c| c.name == path);
        if let [first, second] = words[..] {
            let two = format!("{first} {second}");
            if known(&two) {
                return Some(two);
            }
        }
        words.first().filter(|w| known(w)).map(|w| (*w).to_owned())
    }

    /// `argv` parsed by the binary's own clap.
    pub fn parse(argv: &[String]) -> Cli {
        use clap::Parser;
        let parsed = Cli::try_parse_from(argv);
        assert!(
            parsed.is_ok(),
            "{argv:?} does not parse: {}",
            parsed
                .as_ref()
                .err()
                .map(ToString::to_string)
                .unwrap_or_default()
        );
        parsed.expect("parsed above")
    }

    /// Every value the innermost subcommand of `argv` received: `("", value)` for a
    /// positional, `(long, value)` for an option.
    pub fn positional_and_option_values(argv: &[String]) -> Vec<(String, String)> {
        use clap::CommandFactory;
        let mut command = Cli::command();
        command.build();
        let mut matches = command.clone().try_get_matches_from(argv).unwrap();
        loop {
            let Some((name, sub)) = matches
                .subcommand()
                .map(|(name, sub)| (name.to_owned(), sub.clone()))
            else {
                break;
            };
            command = command.find_subcommand(&name).unwrap().clone();
            matches = sub;
        }
        let mut out = Vec::new();
        for arg in command.get_arguments() {
            let key = if arg.is_positional() {
                String::new()
            } else if let Some(long) = arg.get_long() {
                long.to_owned()
            } else {
                continue;
            };
            if let Ok(Some(raw)) = matches.try_get_raw(arg.get_id().as_str()) {
                out.extend(raw.map(|v| (key.clone(), v.to_string_lossy().into_owned())));
            }
        }
        out
    }

    /// One retail client with `n` addons one release behind: installed in one
    /// transaction, then a scenario where v2 is every addon's latest, with the release
    /// memo aged a day so the next check asks again.
    pub fn behind(n: usize) -> Home {
        let mut home = Home::new(&["wow"]);
        let names: Vec<String> = (0..n).map(|i| format!("addon{i:03}")).collect();
        let base = |name: &str| format!("https://github.com/o/{name}");
        for name in &names {
            gh_release_v(
                &home.http,
                &base(name),
                "v1",
                &Rel::new(name, "v1").assets(),
            );
            gh_latest(&home.http, &base(name), "v1");
        }
        let mut args = vec!["install".to_owned()];
        args.extend(names.iter().map(|name| format!("gh:o/{name}")));
        args.extend(["-t", "retail", "--json"].map(str::to_owned));
        home.exec(&args).expect(0);
        let latest = home.scenario("http-latest");
        for name in &names {
            gh_release_v(&latest, &base(name), "v2", &Rel::new(name, "v2").assets());
            gh_latest(&latest, &base(name), "v2");
        }
        let (mut memo, _) = omaddons::state::memo::load(&paths(&home.env));
        for entry in memo.entries.values_mut() {
            entry.fetched_at -= jiff::SignedDuration::from_hours(24);
        }
        home.seed_memo(&memo);
        home
    }

    /// The numeric value of `var <name> = <expr>` in Model.js, where `<expr>` is a
    /// number or a product of numbers.
    pub fn constant(source: &str, name: &str) -> u64 {
        let line = source
            .lines()
            .find(|l| l.starts_with(&format!("var {name} = ")));
        assert!(line.is_some(), "Model.js defines no {name}");
        let expr = line
            .and_then(|l| l.split_once(" = "))
            .map(|(_, e)| e)
            .unwrap_or_default();
        expr.split('*')
            .map(|f| f.trim().parse::<u64>().unwrap())
            .product()
    }

    /// `$OMADDONS_WIDGET_OUT`, the pipeline's fresh output directory.
    pub fn out_dir() -> PathBuf {
        PathBuf::from(
            std::env::var_os("OMADDONS_WIDGET_OUT")
                .expect("OMADDONS_WIDGET_OUT is set by the widget pipeline"),
        )
    }

    /// The flags the widget puts before every command (`--progress json` for the runs
    /// that report it).
    pub fn standard(progress: bool) -> Vec<String> {
        let mut argv: Vec<String> = ["--json", "--actor", "widget", "--reason"]
            .map(str::to_owned)
            .to_vec();
        argv.push("Fixture (bar widget)".to_owned());
        if progress {
            argv.extend(["--progress".to_owned(), "json".to_owned()]);
        }
        argv
    }

    /// Template `name` with its slots filled, after [`standard`]'s flags. Fixtures only:
    /// `validate_built_argv` never fills a slot itself.
    pub fn fill(
        templates: &BTreeMap<String, Value>,
        name: &str,
        slots: &[(&str, &str)],
    ) -> Vec<String> {
        let template = &templates[name];
        // As Model.buildArgv: every change, and every read on the network lane.
        let mut argv = standard(template["mutates"] == true || template["lane"] == "network");
        for part in template["argv"].as_array().unwrap() {
            let mut part = part.as_str().unwrap().to_owned();
            for (slot, value) in slots {
                part = part.replace(&format!("{{{slot}}}"), value);
            }
            assert!(!part.contains('{'), "{name}: unfilled slot in {part}");
            argv.push(part);
        }
        argv
    }

    /// Writes a run's envelope to `<dir>/<name>.json` and its stderr (NDJSON) to
    /// `<name>.stderr.ndjson`, after checking the exit code and the envelope.
    pub fn write_fixture(dir: &Path, name: &str, run: &Run, code: i32) -> Value {
        let envelope = run.expect(code);
        fs::write(dir.join(format!("{name}.json")), &run.stdout).unwrap();
        if !run.stderr.is_empty() {
            fs::write(dir.join(format!("{name}.stderr.ndjson")), &run.stderr).unwrap();
        }
        envelope
    }

    pub const FOO: &str = "https://github.com/o/foo";
    pub const BAR: &str = "https://github.com/o/bar";
    pub const BAZ: &str = "https://github.com/o/baz";
    pub const LIBFOO: &str = "https://github.com/o/libfoo";

    /// Every fixture `plans_fixtures` writes (M4-D28): the plan runner's scenes, as the
    /// real binary answers them.
    pub const PLAN_FIXTURES: &[&str] = &[
        "plan-list",
        "plan-update-routine",
        "plan-update-routine-apply",
        "plan-update-noop",
        "plan-list-after",
        "plan-update-all",
        "plan-update-all-apply",
        "plan-undo",
        "plan-undo-apply",
        "plan-remove",
        "plan-remove-apply",
        "plan-moved-list",
        "plan-update-moved",
        "plan-apply-stale",
        "plan-update-v4",
        "plan-apply-expired",
        "plan-apply-not-found",
        "plan-asset-list",
        "plan-update-asset",
        "plan-dependency-list",
        "plan-update-dependency",
        "plan-install-dependency",
        "plan-local-edits-list",
        "plan-update-local-edits",
        "plan-removal-list",
        "plan-update-removal",
    ];

    /// Every fixture `repair_fixtures` writes (G2): the doctor's repairs as the real
    /// binary answers the widget's argv, with the context their paths shorten against
    /// and the argv that ran.
    pub const REPAIR_FIXTURES: &[&str] = &[
        "repair-context",
        "repair-argv",
        "repair-status",
        "repair-preview",
        "repair-run",
        "repair-status-after",
        "repair-undo-preview",
        "repair-undo-apply",
        "repair-stale",
        "setaside-status",
        "setaside-explain",
    ];

    /// `argv` (a filled template) as its preview: `--dry-run` after the standard flags.
    pub fn preview_of(mut argv: Vec<String>) -> Vec<String> {
        argv.insert(standard(true).len(), "--dry-run".to_owned());
        argv
    }

    /// The widget's `apply` of plan `id`.
    pub fn apply_of(id: &str) -> Vec<String> {
        let mut argv = standard(true);
        argv.extend(["apply".to_owned(), id.to_owned()]);
        argv
    }

    /// A preview's stored plan: its id, a ULID, offered exactly once as
    /// `omaddons apply <id>`.
    pub fn planned(env: &Value) -> String {
        let id = env["data"]["plan_id"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        assert!(
            id.len() == 26
                && id
                    .chars()
                    .all(|c| c.is_ascii_digit() || c.is_ascii_uppercase()),
            "a plan id: {env}"
        );
        let offers: Vec<&Value> = env["commands"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|c| c["action"] == "apply")
            .collect();
        assert_eq!(offers.len(), 1, "one apply offer: {env}");
        assert_eq!(
            offers[0]["argv"],
            serde_json::json!(["omaddons", "apply", id]),
            "{env}"
        );
        id
    }

    /// A release asset `file` holding each folder's TOC (with `lines` beyond its title
    /// and version) and a `core.lua` per tag: for scenes that need another file name,
    /// several folders, or a TOC that requires something.
    pub struct CustomRel {
        file: String,
        entries: Vec<(String, String)>,
    }

    impl CustomRel {
        pub fn new(file: &str, tag: &str, folders: &[(&str, &[&str])]) -> Self {
            let mut entries = Vec::new();
            for (folder, lines) in folders {
                let title = format!("## Title: {folder}");
                let version = format!("## Version: {tag}");
                let mut all = vec![title.as_str(), version.as_str()];
                all.extend(lines.iter());
                entries.push((format!("{folder}/{folder}.toc"), toc(INTERFACES, &all)));
                entries.push((format!("{folder}/core.lua"), format!("-- {folder} {tag}")));
            }
            Self {
                file: file.to_owned(),
                entries,
            }
        }

        pub fn assets(&self) -> [Asset<'_>; 1] {
            [(
                self.file.as_str(),
                FLAVORS,
                self.entries
                    .iter()
                    .map(|(p, t)| (p.as_str(), t.as_str()))
                    .collect(),
            )]
        }
    }

    /// Retail and Classic Era.
    pub const INTERFACES: &str = "11509, 120105";
    pub const FLAVORS: &[(&str, u32)] = &[("mainline", 120_105), ("classic", 11_509)];

    /// A release's one asset: `<Name>.zip` with `<Name>/<Name>.toc` and a `core.lua`
    /// that differs per tag.
    pub struct Rel {
        file: String,
        toc_path: String,
        toc: String,
        core_path: String,
        core: String,
    }

    impl Rel {
        pub fn new(name: &str, tag: &str) -> Self {
            let title = format!("## Title: {name}");
            let version = format!("## Version: {tag}");
            Self {
                file: format!("{name}.zip"),
                toc_path: format!("{name}/{name}.toc"),
                toc: toc(INTERFACES, &[&title, &version]),
                core_path: format!("{name}/core.lua"),
                core: format!("-- {name} {tag}"),
            }
        }

        /// [`Rel::new`] whose TOC also holds `line` (an `## X-Website:` naming its
        /// repository, for `adopt`'s evidence).
        pub fn with_line(name: &str, tag: &str, line: &str) -> Self {
            let mut rel = Self::new(name, tag);
            let (title, version) = (format!("## Title: {name}"), format!("## Version: {tag}"));
            rel.toc = toc(INTERFACES, &[&title, &version, line]);
            rel
        }

        /// Puts the release's folder into `addons` by hand, as another manager or an
        /// unzip would (`core` replaces its `core.lua` when given: edited files).
        pub fn place(&self, addons: &std::path::Path, core: Option<&str>) {
            let toc_path = addons.join(&self.toc_path);
            fs::create_dir_all(toc_path.parent().unwrap()).unwrap();
            fs::write(&toc_path, &self.toc).unwrap();
            fs::write(addons.join(&self.core_path), core.unwrap_or(&self.core)).unwrap();
        }

        pub fn assets(&self) -> [Asset<'_>; 1] {
            [(
                self.file.as_str(),
                FLAVORS,
                vec![
                    (self.toc_path.as_str(), self.toc.as_str()),
                    (self.core_path.as_str(), self.core.as_str()),
                ],
            )]
        }
    }
}

/// Runs `argv` on `home` through the log, writes its envelope as fixture `name`
/// (expecting exit `code`), logs it, and returns it.
#[cfg(test)]
fn plan_fixture(
    log: &TestLog,
    home: &Home,
    dir: &std::path::Path,
    name: &str,
    argv: &[String],
    code: i32,
) -> Value {
    let ran = log.run(home, argv);
    let envelope = write_fixture(dir, name, &ran.run, code);
    log.json(name, &envelope);
    envelope
}

/// The addon `name`'s row on client `id` in a `list` envelope.
#[cfg(test)]
fn list_row<'a>(list: &'a Value, id: &str, name: &str) -> &'a Value {
    let target = list["data"]["targets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == id);
    let row = target.and_then(|t| {
        t["addons"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["name"] == name)
    });
    assert!(row.is_some(), "no {name} on {id}: {list}");
    row.unwrap_or(&Value::Null)
}

/// The plan runner's scenes (M4-D28), from the real binary, for the bun and QML
/// scenarios: previews, their `apply`, and `apply`'s refusals. Each fixture's shape is
/// asserted before it is written; `PLAN_FIXTURES` lists exactly what is written.
#[cfg(test)]
fn plans_fixtures(dir: &std::path::Path, t: &BTreeMap<String, Value>) {
    use common::http::gh_release_v;
    use support::{CustomRel, PLAN_FIXTURES, apply_of, planned, preview_of};

    let log = tlog!();
    let foo = |tag: &str| Rel::new("Foo", tag);
    let update_foo = || {
        preview_of(fill(
            t,
            "update_addon",
            &[("target", "retail"), ("addon", "Foo")],
        ))
    };

    log.step("scene A: routine, Update all, undo and remove through apply");
    let mut home = Home::new(&["wow", "wow_classic_era"]);
    for (base, name) in [(support::FOO, "Foo"), (support::BAR, "Bar")] {
        let (old, new) = (Rel::new(name, "v1"), Rel::new(name, "v2"));
        home.installed_behind(
            base,
            ("v1", &old.assets()),
            ("v2", &new.assets()),
            &["retail", "era"],
        );
    }
    log.run(&home, &fill(t, "check", &[])).ok();
    let list = plan_fixture(&log, &home, dir, "plan-list", &fill(t, "list", &[]), 0);
    let latest = &list_row(&list, "retail", "Foo")["latest"];
    assert_eq!(
        (&latest["source"], &latest["release_id"], &latest["asset"]),
        (&"github".into(), &"gh:o/foo@v2".into(), &"Foo.zip".into()),
        "the row names its release: {list}"
    );
    let preview = plan_fixture(&log, &home, dir, "plan-update-routine", &update_foo(), 0);
    assert_eq!(preview["data"]["approval"]["routine"], true, "{preview}");
    let id = planned(&preview);
    log.note("plan-update-routine: approval.routine true, one apply offer");
    let applied = plan_fixture(
        &log,
        &home,
        dir,
        "plan-update-routine-apply",
        &apply_of(&id),
        0,
    );
    assert_eq!(applied["data"]["applied"]["command"], "update", "{applied}");
    assert_eq!(
        applied["data"]["outcomes"][0]["status"], "committed",
        "{applied}"
    );
    let noop = plan_fixture(&log, &home, dir, "plan-update-noop", &update_foo(), 0);
    assert!(noop["data"]["plan_id"].is_null(), "{noop}");
    assert_eq!(
        noop["data"]["approval"]["selection"],
        serde_json::json!([]),
        "{noop}"
    );
    assert!(
        noop["commands"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["action"] != "apply"),
        "no apply offer: {noop}"
    );
    plan_fixture(
        &log,
        &home,
        dir,
        "plan-list-after",
        &fill(t, "list", &[]),
        0,
    );
    let all = plan_fixture(
        &log,
        &home,
        dir,
        "plan-update-all",
        &preview_of(fill(t, "update_all", &[])),
        0,
    );
    assert_eq!(all["data"]["approval"]["routine"], true, "{all}");
    assert_eq!(
        all["data"]["approval"]["selection"]
            .as_array()
            .unwrap()
            .len(),
        3,
        "{all}"
    );
    let id = planned(&all);
    let applied = plan_fixture(&log, &home, dir, "plan-update-all-apply", &apply_of(&id), 0);
    let outcomes = applied["data"]["outcomes"].as_array().unwrap();
    assert!(
        outcomes.len() == 2 && outcomes.iter().all(|o| o["status"] == "committed"),
        "a committed outcome per client: {applied}"
    );
    let status = log.run(&home, &fill(t, "status", &[]));
    let status = status.ok().envelope().clone();
    let mut undo = support::standard(true);
    undo.push("--dry-run".to_owned());
    undo.extend(
        support::strings(&status["data"]["last_change"]["undo"]["argv"])
            .into_iter()
            .skip(1),
    );
    let preview = plan_fixture(&log, &home, dir, "plan-undo", &undo, 0);
    let id = planned(&preview);
    assert_eq!(preview["commands"][0]["destructive"], true, "{preview}");
    let applied = plan_fixture(&log, &home, dir, "plan-undo-apply", &apply_of(&id), 0);
    assert_eq!(applied["data"]["applied"]["command"], "undo", "{applied}");
    let remove = preview_of(fill(t, "remove", &[("target", "retail"), ("addon", "Bar")]));
    let preview = plan_fixture(&log, &home, dir, "plan-remove", &remove, 0);
    let id = planned(&preview);
    assert_eq!(preview["commands"][0]["destructive"], true, "{preview}");
    plan_fixture(&log, &home, dir, "plan-remove-apply", &apply_of(&id), 0);

    log.step("scene B: a release that moves; a stale, an expired and a missing plan");
    let mut home = Home::new(&["wow"]);
    home.installed_behind(
        support::FOO,
        ("v1", &foo("v1").assets()),
        ("v2", &foo("v2").assets()),
        &["retail"],
    );
    log.run(&home, &fill(t, "check", &[])).ok();
    plan_fixture(
        &log,
        &home,
        dir,
        "plan-moved-list",
        &fill(t, "list", &[]),
        0,
    );
    let landed = home.next_scenario();
    gh_release_v(&landed, support::FOO, "v3", &foo("v3").assets());
    gh_latest(&landed, support::FOO, "v3");
    // The update's dry run answers from the memo for 15 minutes: refresh it.
    log.run(&home, &["outdated", "--refresh", "--json"]).ok();
    let moved = plan_fixture(&log, &home, dir, "plan-update-moved", &update_foo(), 0);
    assert_eq!(
        moved["data"]["approval"]["selection"][0]["to_version"], "v3",
        "{moved}"
    );
    let id = planned(&moved);
    let landed = home.next_scenario();
    gh_release_v(&landed, support::FOO, "v4", &foo("v4").assets());
    gh_latest(&landed, support::FOO, "v4");
    let stale = plan_fixture(&log, &home, dir, "plan-apply-stale", &apply_of(&id), 1);
    assert_eq!(stale["error"]["code"], "plan_stale", "{stale}");
    assert!(
        support::strings(&stale["error"]["details"]["changed"]).contains(&"selection".to_owned()),
        "{stale}"
    );
    let v4 = plan_fixture(&log, &home, dir, "plan-update-v4", &update_foo(), 0);
    assert_eq!(
        v4["data"]["approval"]["selection"][0]["to_version"], "v4",
        "{v4}"
    );
    let id = planned(&v4);
    // 25 hours after the preview: an hour past the plan's expiry.
    let expires: jiff::Timestamp = v4["data"]["plan_expires_at"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let later = expires + jiff::SignedDuration::from_hours(1);
    let ran = log
        .cmd(&home, &apply_of(&id))
        .env("OMADDONS_NOW", later.to_string())
        .run();
    let expired = write_fixture(dir, "plan-apply-expired", &ran.run, 1);
    log.json("plan-apply-expired", &expired);
    assert_eq!(
        (
            &expired["error"]["code"],
            &expired["error"]["details"]["reason"]
        ),
        (&"plan_stale".into(), &"expired".into()),
        "{expired}"
    );
    let missing = plan_fixture(
        &log,
        &home,
        dir,
        "plan-apply-not-found",
        &apply_of("01ZZZZZZZZZZZZZZZZZZZZZZZZ"),
        3,
    );
    assert_eq!(missing["error"]["code"], "plan_not_found", "{missing}");

    log.step("scene C: equal versions, another asset");
    let mut home = Home::new(&["wow"]);
    home.installed_behind(
        support::FOO,
        ("v1", &foo("v1").assets()),
        ("v2", &foo("v2").assets()),
        &["retail"],
    );
    log.run(&home, &fill(t, "check", &[])).ok();
    plan_fixture(
        &log,
        &home,
        dir,
        "plan-asset-list",
        &fill(t, "list", &[]),
        0,
    );
    let reuploaded = home.next_scenario();
    let retail = CustomRel::new("Foo-retail.zip", "v2", &[("Foo", &[])]);
    gh_release_v(&reuploaded, support::FOO, "v2", &retail.assets());
    gh_latest(&reuploaded, support::FOO, "v2");
    log.run(&home, &["outdated", "--refresh", "--json"]).ok();
    let asset = plan_fixture(&log, &home, dir, "plan-update-asset", &update_foo(), 0);
    let item = &asset["data"]["approval"]["selection"][0];
    assert_eq!(
        (&item["to_version"], &item["release_id"], &item["asset"]),
        (
            &"v2".into(),
            &"gh:o/foo@v2".into(),
            &"Foo-retail.zip".into()
        ),
        "{asset}"
    );
    planned(&asset);

    log.step("scene D: a dependency added");
    let mut home = Home::new(&["wow"]);
    let catalog = home.env.config().join("omaddons/catalog.d");
    fs::create_dir_all(&catalog).unwrap();
    fs::write(
        catalog.join("libs.toml"),
        "schema_version = \"1.0\"\n\n[[addon]]\nname = \"LibFoo\"\nref = \"gh:o/libfoo\"\nfolders = [\"LibFoo\"]\n",
    )
    .unwrap();
    let requiring = CustomRel::new("Foo.zip", "v2", &[("Foo", &["## Dependencies: LibFoo"])]);
    let behind = home.installed_behind(
        support::FOO,
        ("v1", &foo("v1").assets()),
        ("v2", &requiring.assets()),
        &["retail"],
    );
    let lib = CustomRel::new("LibFoo.zip", "1.0", &[("LibFoo", &[])]);
    gh_release_v(&behind, support::LIBFOO, "1.0", &lib.assets());
    gh_latest(&behind, support::LIBFOO, "1.0");
    log.run(&home, &fill(t, "check", &[])).ok();
    plan_fixture(
        &log,
        &home,
        dir,
        "plan-dependency-list",
        &fill(t, "list", &[]),
        0,
    );
    let dependency = plan_fixture(&log, &home, dir, "plan-update-dependency", &update_foo(), 0);
    let approval = &dependency["data"]["approval"];
    assert!(
        approval["routine"] == false
            && support::strings(&approval["reasons"]).contains(&"dependency_added".to_owned())
            && support::strings(&approval["scope"]["dependencies_added"]) == ["LibFoo"],
        "{dependency}"
    );
    planned(&dependency);
    // An install from Search or an addon page (1n2p.25): Bar is new here, and its TOC
    // requires LibFoo, which the preview adds.
    let bar = CustomRel::new("Bar.zip", "v1", &[("Bar", &["## Dependencies: LibFoo"])]);
    gh_release_v(&behind, support::BAR, "v1", &bar.assets());
    gh_latest(&behind, support::BAR, "v1");
    let install = plan_fixture(
        &log,
        &home,
        dir,
        "plan-install-dependency",
        &preview_of(fill(
            t,
            "install",
            &[("target", "retail"), ("ref", "gh:o/bar")],
        )),
        0,
    );
    let approval = &install["data"]["approval"];
    assert!(
        support::strings(&approval["reasons"]).contains(&"dependency_added".to_owned())
            && support::strings(&approval["scope"]["dependencies_added"]) == ["LibFoo"],
        "{install}"
    );
    planned(&install);

    log.step("scene E: local edits replaced");
    let mut home = Home::new(&["wow"]);
    home.installed_behind(
        support::FOO,
        ("v1", &foo("v1").assets()),
        ("v2", &foo("v2").assets()),
        &["retail"],
    );
    fs::write(
        home.addons("_retail_").join("Foo/core.lua"),
        "-- edited by hand\n",
    )
    .unwrap();
    log.run(&home, &fill(t, "check", &[])).ok();
    plan_fixture(
        &log,
        &home,
        dir,
        "plan-local-edits-list",
        &fill(t, "list", &[]),
        0,
    );
    let edits = plan_fixture(
        &log,
        &home,
        dir,
        "plan-update-local-edits",
        &update_foo(),
        0,
    );
    let scope = &edits["data"]["approval"]["scope"]["local_changes_replaced"];
    assert!(
        support::strings(&edits["data"]["approval"]["reasons"])
            .contains(&"local_changes_replaced".to_owned())
            && scope["count"].as_u64().unwrap_or(0) >= 1
            && support::strings(&scope["folders"]).contains(&"Foo".to_owned()),
        "{edits}"
    );
    planned(&edits);

    log.step("scene F: a folder the update removes");
    let mut home = Home::new(&["wow"]);
    let two = CustomRel::new("Foo.zip", "v1", &[("Foo", &[]), ("Foo_Options", &[])]);
    let one = CustomRel::new("Foo.zip", "v2", &[("Foo", &[])]);
    home.installed_behind(
        support::FOO,
        ("v1", &two.assets()),
        ("v2", &one.assets()),
        &["retail"],
    );
    log.run(&home, &fill(t, "check", &[])).ok();
    plan_fixture(
        &log,
        &home,
        dir,
        "plan-removal-list",
        &fill(t, "list", &[]),
        0,
    );
    let removal = plan_fixture(&log, &home, dir, "plan-update-removal", &update_foo(), 0);
    let approval = &removal["data"]["approval"];
    assert!(
        support::strings(&approval["reasons"]).contains(&"folders_removed".to_owned())
            && support::strings(&approval["scope"]["folders_removed"])
                .contains(&"Foo_Options".to_owned()),
        "{removal}"
    );
    planned(&removal);

    let mut written: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| n.starts_with("plan-") && n.ends_with(".json"))
        .map(|n| n.trim_end_matches(".json").to_owned())
        .collect();
    written.sort();
    let mut listed: Vec<String> = PLAN_FIXTURES.iter().map(|n| (*n).to_owned()).collect();
    listed.sort();
    assert_eq!(
        written, listed,
        "plans_fixtures writes exactly PLAN_FIXTURES"
    );
}

/// The doctor's repairs as the widget runs them (G2), from the real binary, for the bun
/// cases and `tst_repairs.qml`: the credential repair previewed, run bound to its token
/// and undone through `undo`'s stored plan (scene A); the same repair gone stale (scene
/// B); and a set-aside row with its explanation (scene C). Every argv is the one the
/// widget builds. The failure modes are planted by the doctor's own fixtures.
/// `REPAIR_FIXTURES` lists exactly what is written.
#[cfg(test)]
fn repair_fixtures(dir: &std::path::Path, t: &BTreeMap<String, Value>) {
    use std::os::unix::fs::PermissionsExt;

    use common::doctor::{check_envelope, plant};
    use serde_json::json;
    use support::{REPAIR_FIXTURES, apply_of, planned, preview_of, standard};

    // ubs:ignore — failure-mode ids, not secrets
    const CREDENTIAL: &str = "fm-secrets-credential-file-mode";
    const SET_ASIDE: &str = "fm-concurrency_primitives-journal-too-new";

    let log = tlog!();
    let mode_of =
        |path: &std::path::Path| fs::metadata(path).unwrap().permissions().mode() & 0o7777;
    // A doctor run: its envelope checked against the doctor's contract, then written.
    let doctor_fixture = |home: &Home, name: &str, argv: &[String], code: i32| -> Value {
        let ran = log.run(home, argv);
        check_envelope(ran.stdout(), ran.stderr(), ran.run.code);
        let envelope = write_fixture(dir, name, &ran.run, code);
        log.json(name, &envelope);
        envelope
    };
    // The widget's bound run of a filled template: its token right after the flags.
    let bound_of = |argv: &[String], token: &str| -> Vec<String> {
        let mut argv = argv.to_vec();
        let at = standard(true).len();
        argv.splice(at..at, ["--if-match".to_owned(), token.to_owned()]);
        argv
    };
    let token_of = |preview: &Value| -> String {
        let token = preview["precondition"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        assert!(
            token.len() == 68
                && token.starts_with("pc1:")
                && token[4..]
                    .chars()
                    .all(|c| matches!(c, '0'..='9' | 'a'..='f')),
            "a preview token: {preview}"
        );
        token
    };
    let issue_in = |list: &Value, id: &str| -> Value {
        let found = list
            .as_array()
            .into_iter()
            .flatten()
            .find(|i| i["id"] == id)
            .cloned();
        assert!(found.is_some(), "no {id} in {list}");
        found.unwrap_or(Value::Null)
    };

    log.step("repair: plant");
    let home = Home::new(&["wow"]);
    log.env(&home);
    // The client discovered (and the discovery cache written), as a widget sees it.
    log.run(&home, &fill(t, "status_full", &[])).ok();
    let file = plant::credential_file_mode(&home);
    let config = home.env.config();
    log.tree("config", &config);
    log.note(format!("credential mode {:04o}", mode_of(&file)));
    assert_eq!(mode_of(&file), 0o644);
    let context = json!({
        "HOME": home.env.home(), "XDG_CONFIG_HOME": config, "XDG_STATE_HOME": home.env.state(),
        "XDG_CACHE_HOME": home.env.cache(), "XDG_DATA_HOME": home.env.data(),
    });
    fs::write(dir.join("repair-context.json"), context.to_string()).unwrap();

    log.step("repair: status");
    let status = plan_fixture(
        &log,
        &home,
        dir,
        "repair-status",
        &fill(t, "status", &[]),
        0,
    );
    let issue = issue_in(&status["data"]["issues"], CREDENTIAL);
    assert_eq!(issue["auto_fixable"], true, "{issue}");
    assert_eq!(
        issue["remediation"]["argv"],
        json!(["omaddons", "doctor", "fix", "--only", CREDENTIAL]),
        "{issue}"
    );
    assert_eq!(
        issue["explain"]["argv"],
        json!(["omaddons", "doctor", "explain", CREDENTIAL]),
        "{issue}"
    );

    log.step("repair: preview");
    let fix = fill(t, "doctor_fix", &[("fm", CREDENTIAL)]);
    let preview_argv = preview_of(fix.clone());
    let preview = doctor_fixture(&home, "repair-preview", &preview_argv, 1);
    assert_eq!(
        (&preview["dry_run"], &preview["state"], &preview["tx"]),
        (&json!(true), &json!("DONE_FINDINGS"), &Value::Null),
        "{preview}"
    );
    let actions = preview["actions_planned"].as_array().unwrap();
    assert_eq!(actions.len(), 1, "one planned action: {preview}");
    assert_eq!(
        (
            &actions[0]["op"],
            &actions[0]["mode"],
            &actions[0]["pre"]["mode"],
            &actions[0]["fm"]
        ),
        (
            &json!("chmod"),
            &json!("0600"),
            &json!("0644"),
            &json!(CREDENTIAL)
        ),
        "{preview}"
    );
    let token = token_of(&preview);
    log.assert_unchanged("config", &config);

    log.step("repair: bound run");
    let bound_argv = bound_of(&fix, &token);
    let run = doctor_fixture(&home, "repair-run", &bound_argv, 0);
    assert_eq!(run["state"], "DONE_OK", "{run}");
    assert!(run["tx"].is_string(), "{run}");
    assert_eq!(run["actions"][0]["ok"], true, "{run}");
    assert!(run["undo_argv"].is_array(), "{run}");
    log.note(format!("credential mode {:04o}", mode_of(&file)));
    assert_eq!(mode_of(&file), 0o600);
    // What ran, without the binary: bun builds the same from the same `fix`.
    let ran = json!({ "preview": preview_argv, "bound": bound_argv });
    fs::write(dir.join("repair-argv.json"), ran.to_string()).unwrap();

    log.step("repair: status after");
    let after = plan_fixture(
        &log,
        &home,
        dir,
        "repair-status-after",
        &fill(t, "status", &[]),
        0,
    );
    assert!(
        after["data"]["issues"]
            .as_array()
            .into_iter()
            .flatten()
            .all(|i| i["id"] != CREDENTIAL), // ubs:ignore — a failure-mode id, not a secret
        "repaired: {after}"
    );
    let change = &after["data"]["last_change"];
    assert_eq!(
        (&change["kind"], &after["data"]["last_change_state"]),
        (&json!("repair"), &json!("ok")),
        "{after}"
    );
    let undo = strings(&change["undo"]["argv"]);
    assert_eq!(&undo[..3], ["omaddons", "undo", "--run"], "{after}");

    log.step("repair: undo preview");
    let mut undo_preview = standard(true);
    undo_preview.push("--dry-run".to_owned());
    undo_preview.extend(undo.into_iter().skip(1));
    let undo_env = plan_fixture(&log, &home, dir, "repair-undo-preview", &undo_preview, 0);
    let reverts = &undo_env["data"]["repairs"][0];
    assert_eq!(
        (
            &undo_env["data"]["kind"],
            &reverts["op"],
            &reverts["octal_mode"]
        ),
        (&json!("repair"), &json!("chmod"), &json!("0644")),
        "{undo_env}"
    );
    assert!(undo_env["data"]["approval"].is_object(), "{undo_env}");
    let id = planned(&undo_env);

    log.step("repair: apply");
    let applied = plan_fixture(&log, &home, dir, "repair-undo-apply", &apply_of(&id), 0);
    assert_eq!(applied["data"]["applied"]["command"], "undo", "{applied}");
    log.note(format!("credential mode {:04o}", mode_of(&file)));
    assert_eq!(mode_of(&file), 0o644);
    log.assert_unchanged("config", &config);

    // Scene B: the same home, the file 0644 again; it changes between preview and Yes.
    log.step("stale: preview");
    let first = log.run(&home, &preview_of(fix.clone()));
    let first = check_envelope(first.stdout(), first.stderr(), first.run.code);
    assert_eq!(first["exit_code"], 1, "{first}");
    let token = token_of(&first);
    log.note(format!("stale: previewed with {token}"));

    log.step("stale: the file changes");
    fs::set_permissions(&file, fs::Permissions::from_mode(0o640)).unwrap();
    log.note(format!(
        "credential mode {:04o} (a dotfiles sync)",
        mode_of(&file)
    ));

    log.step("stale: bound run");
    let stale = doctor_fixture(&home, "repair-stale", &bound_of(&fix, &token), 4);
    let error = &stale["error"];
    assert_eq!(
        (&error["code"], &error["retryable"]),
        (&json!("plan_stale"), &json!(false)),
        "{stale}"
    );
    assert!(
        strings(&error["details"]["changed"]).contains(&"observed".to_owned()),
        "{stale}"
    );
    assert!(
        strings(&error["fix_argv"]).contains(&"--dry-run".to_owned()),
        "the fix previews again: {stale}"
    );
    assert_eq!(mode_of(&file), 0o640, "nothing ran");

    // Scene C: retail's interrupted change from a newer omaddons, last, in the same home
    // (it refuses every later change on retail; nothing changes after it), so its paths
    // shorten against the same `repair-context`. The token file is made private by
    // hand first, so its status lists the set-aside alone.
    log.step("set-aside: plant");
    fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
    log.note(format!(
        "credential mode {:04o} (made private by hand)",
        mode_of(&file)
    ));
    let fixtures = crate::fixture::fixtures::all();
    let too_new = fixtures.iter().find(|f| f.id() == SET_ASIDE);
    assert!(too_new.is_some(), "the doctor's {SET_ASIDE} fixture");
    too_new.unwrap().corrupt(&home);
    let client = home.client("_retail_");
    log.tree("state", &home.state());
    log.tree("client", &client);

    log.step("set-aside: status");
    let status = plan_fixture(
        &log,
        &home,
        dir,
        "setaside-status",
        &fill(t, "status", &[]),
        0,
    );
    let retail = status["data"]["targets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == "retail")
        .cloned()
        .unwrap_or(Value::Null);
    let issue = issue_in(&retail["issues"], SET_ASIDE);
    let forced = [
        "omaddons", "doctor", "fix", "--only", SET_ASIDE, "-t", "retail", "--force", "--yes",
    ];
    assert_eq!(
        (
            &issue["severity"],
            &issue["auto_fixable"],
            &issue["remediation"]["argv"]
        ),
        (&json!("P0"), &json!(false), &json!(forced)),
        "{issue}"
    );

    log.step("set-aside: explain");
    let mut explain = standard(false);
    explain.extend(strings(&issue["explain"]["argv"]).into_iter().skip(1));
    let explained = doctor_fixture(&home, "setaside-explain", &explain, 0);
    assert_eq!(explained["failure_mode"]["id"], SET_ASIDE, "{explained}");
    assert_eq!(
        explained["instances"][0]["remediation"]["command"],
        forced.join(" "),
        "{explained}"
    );
    assert!(
        !explained["instances"].to_string().contains("/WTF/"),
        "no evidence below WTF/: {explained}"
    );
    log.assert_unchanged("state", &home.state());
    log.assert_unchanged("client", &client);

    // The rows the widget's output-cap bound assumes (the_output_cap_bounds_every_template).
    log.step("row sizes");
    let largest = |list: &Value| -> u64 {
        list.as_array()
            .into_iter()
            .flatten()
            .map(|row| row.to_string().len() as u64)
            .max()
            .unwrap_or(0)
    };
    let sizes = json!({
        "action": largest(&preview["actions_planned"]),
        "finding": largest(&preview["findings"]),
        "instance": largest(&explained["instances"]),
    });
    log.json("row sizes", &sizes);
    assert!(
        sizes["action"].as_u64() <= Some(support::ACTION_ROW),
        "an action row past ACTION_ROW: {sizes}"
    );
    for key in ["finding", "instance"] {
        assert!(
            sizes[key].as_u64() <= Some(support::FINDING_ROW),
            "a {key} row past FINDING_ROW: {sizes}"
        );
    }

    let mut written: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| {
            (n.starts_with("repair-") || n.starts_with("setaside-")) && n.ends_with(".json")
        })
        .map(|n| n.trim_end_matches(".json").to_owned())
        .collect();
    written.sort();
    let mut listed: Vec<String> = REPAIR_FIXTURES.iter().map(|n| (*n).to_owned()).collect();
    listed.sort();
    assert_eq!(
        written, listed,
        "repair_fixtures writes exactly REPAIR_FIXTURES"
    );
}

/// Envelopes from the real binary for bun's reducer tests, written to
/// Installs `r` at `version` on retail from a local zip shipping `folders`.
#[cfg(test)]
fn install_zip(home: &Home, r: &str, version: &str, folders: &[&str]) {
    use common::addons::toc;
    use common::archives::zip_text;

    let dl = home.env.root().join("dl");
    fs::create_dir_all(&dl).unwrap();
    let zip = dl.join(format!("{}-{version}.zip", r.replace([':', '/'], "-")));
    let line = format!("## Version: {version}");
    let tocs: Vec<(String, String)> = folders
        .iter()
        .map(|f| (format!("{f}/{f}.toc"), toc("120105", &[&line])))
        .collect();
    let entries: Vec<(&str, &str)> = tocs.iter().map(|(p, t)| (p.as_str(), t.as_str())).collect();
    fs::write(&zip, zip_text(&entries)).unwrap();
    let zip = zip.display().to_string();
    home.run(&["install", r, "--file", &zip, "-t", "retail"])
        .expect(0);
}

/// Retail's `WTF/` (WeakAuras's and Foo's settings, `Config.wtf`) backed up through the
/// widget's `backup_target` with WeakAuras 5.19.2 installed, then WeakAuras updated to
/// 5.20.0 and a session played (both settings files rewritten). The home, its `WTF/`
/// and the backup's id.
#[cfg(test)]
fn restore_scene(t: &BTreeMap<String, Value>) -> (Home, std::path::PathBuf, String) {
    use common::wtf::Wtf;

    let home = Home::new(&["wow"]);
    let wtf = Wtf::at(home.client("_retail_"))
        .config_wtf("SET locale \"enUS\"\n")
        .saved_variables("WeakAuras", 400)
        .saved_variables("Foo", 200)
        .build();
    install_zip(
        &home,
        "gh:o/WeakAuras",
        "5.19.2",
        &["WeakAuras", "WeakAurasOptions"],
    );
    let created = home
        .exec(&fill(t, "backup_target", &[("target", "retail")]))
        .expect(0);
    let id = created["data"]["targets"][0]["backup"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    install_zip(
        &home,
        "gh:o/WeakAuras",
        "5.20.0",
        &["WeakAuras", "WeakAurasOptions"],
    );
    let account = wtf.join("Account/FAKEACCT/SavedVariables");
    fs::write(
        account.join("WeakAuras.lua"),
        "WeakAurasDB = { played = 1 }\n",
    )
    .unwrap();
    fs::write(account.join("Foo.lua"), "FooDB = { played = true }\n").unwrap();
    (home, wtf, id)
}

/// The chooser's "<addon>'s settings" end to end on a fixture home: the `restore_addon`
/// template's dry run, then the run bound to its token. WeakAuras's settings come back;
/// Foo's file and `Config.wtf` stay exactly as they were.
#[test]
fn the_choosers_addon_restore_leaves_every_other_setting_alone() {
    let t = templates();
    let (home, wtf, id) = restore_scene(&t);
    let account = wtf.join("Account/FAKEACCT/SavedVariables");
    let played = b"WeakAurasDB = { played = 1 }\n".to_vec();
    let foo = fs::read(account.join("Foo.lua")).unwrap();
    let config = fs::read(wtf.join("Config.wtf")).unwrap();
    let slots = [
        ("target", "retail"),
        ("addon", "WeakAuras"),
        ("backup", id.as_str()),
    ];
    let mut preview = fill(&t, "restore_addon", &slots);
    let at = preview.iter().position(|a| a == "backup").unwrap();
    preview.insert(at, "--dry-run".to_owned());
    let dry = home.exec(&preview).expect(0);
    let token = dry["data"]["precondition"].as_str().unwrap().to_owned();
    assert_eq!(fs::read(account.join("WeakAuras.lua")).unwrap(), played);
    let mut real = fill(&t, "restore_addon", &slots);
    let at = real.iter().position(|a| a == "backup").unwrap();
    real.splice(at..at, ["--if-match".to_owned(), token]);
    let done = home.exec(&real).expect(0);
    assert_eq!(done["data"]["mode"], "addons", "{done}");
    let restored = fs::read(account.join("WeakAuras.lua")).unwrap();
    assert!(restored.starts_with(b"WeakAurasDB = { [\""), "{restored:?}");
    assert_eq!(fs::read(account.join("Foo.lua")).unwrap(), foo);
    assert_eq!(fs::read(wtf.join("Config.wtf")).unwrap(), config);
}

/// The Backups view's and the restore chooser's envelopes: retail's `WTF/` (WeakAuras's
/// and Foo's settings, `Config.wtf`) backed up with WeakAuras 5.19.2 installed, then
/// The views' states (`omad-widget-user-first-1n2p.19`): a check that could not reach
/// GitHub for Foo, after one that could (Bar answers both times), and a search whose
/// online sources were skipped (`--offline`, the shape a rate-limited or turned-off
/// source gives too).
#[cfg(test)]
fn states_fixtures(dir: &std::path::Path, t: &BTreeMap<String, Value>) {
    let mut home = Home::new(&["wow"]);
    for (base, name) in [(support::FOO, "Foo"), (support::BAR, "Bar")] {
        let (old, new) = (Rel::new(name, "v1"), Rel::new(name, "v2"));
        home.installed_behind(
            base,
            ("v1", &old.assets()),
            ("v2", &new.assets()),
            &["retail"],
        );
    }
    home.exec(&fill(t, "check", &[])).expect(0);
    gh_latest_status(&home.http, support::FOO, 503);
    let mut check = fill(t, "check", &[]);
    check.push("--refresh".to_owned());
    let unreachable = write_fixture(dir, "check-unreachable", &home.exec(&check), 0);
    assert_eq!(unreachable["partial"], true, "{unreachable}");

    let mut search = fill(
        t,
        "search",
        &[("target", "retail"), ("query", "damage meter")],
    );
    search.insert(search.len() - 2, "--offline".to_owned());
    let skipped = write_fixture(dir, "search-skipped", &home.exec(&search), 0);
    assert!(
        !skipped["data"]["sources_skipped"]
            .as_array()
            .unwrap()
            .is_empty(),
        "{skipped}"
    );
}

/// The take-over flow (1n2p.32): folders put into Retail's AddOns by hand, as another
/// manager leaves them. Foo is exactly what its newest GitHub release installs (its TOC
/// names the repository), Bar is named by its TOC but has a file changed, and Loose has
/// nothing that identifies it. Then `status` (they are adoptable), the widget's take-over
/// preview (`adopt --apply --dry-run`, a stored plan), its `apply`, and `status` after.
#[cfg(test)]
fn adopt_fixtures(dir: &std::path::Path, t: &BTreeMap<String, Value>) {
    use support::{apply_of, planned, preview_of};

    let log = tlog!();
    let home = Home::new(&["wow"]);
    let addons = home.addons("_retail_");
    let foo = Rel::with_line("Foo", "v1", "## X-Website: https://github.com/o/foo");
    let bar = Rel::with_line("Bar", "v1", "## X-Website: https://github.com/o/bar");
    for (base, rel) in [(support::FOO, &foo), (support::BAR, &bar)] {
        gh_release(&home.http, base, "v1", &rel.assets());
        gh_latest(&home.http, base, "v1");
    }
    foo.place(&addons, None);
    bar.place(&addons, Some("-- Bar v1, edited by hand"));
    let loose = addons.join("Loose");
    fs::create_dir_all(&loose).unwrap();
    fs::write(
        loose.join("Loose.toc"),
        "## Interface: 120105\n## Title: Loose\n",
    )
    .unwrap();
    home.run(&["status"]).expect(0);
    let status = write_fixture(
        dir,
        "status-adoptable",
        &home.exec(&fill(t, "status", &[])),
        0,
    );
    let retail = &status["data"]["targets"][0];
    assert_eq!(
        (&retail["adoptable"], &retail["addons"]["total"]),
        (&serde_json::json!(0), &serde_json::json!(0)),
        "no catalog entry names these folders, and nothing is managed yet: {status}"
    );
    // The Installed view's take-over row reads them here.
    let list = write_fixture(dir, "list-adoptable", &home.exec(&fill(t, "list", &[])), 0);
    assert_eq!(
        list["data"]["targets"][0]["counts"]["unmanaged"],
        serde_json::json!(3),
        "{list}"
    );
    let preview = plan_fixture(
        &log,
        &home,
        dir,
        "adopt-preview",
        &preview_of(fill(t, "adopt_target", &[("target", "retail")])),
        0,
    );
    let selection: Vec<(&str, &str)> = preview["data"]["approval"]["selection"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| (s["addon"].as_str().unwrap(), s["op"].as_str().unwrap()))
        .collect();
    assert_eq!(selection, [("Bar", "adopt"), ("Foo", "adopt")], "{preview}");
    let id = planned(&preview);
    plan_fixture(&log, &home, dir, "adopt-apply", &apply_of(&id), 0);
    let after = write_fixture(
        dir,
        "status-adopted",
        &home.exec(&fill(t, "status", &[])),
        0,
    );
    assert_eq!(
        after["data"]["targets"][0]["addons"]["total"],
        serde_json::json!(2),
        "Foo and Bar are managed, Loose is not: {after}"
    );
    assert!(loose.join("Loose.toc").is_file(), "Loose stays as it was");
}

/// The first run (1n2p.32): a home where discovery finds no client, `targets add` refused
/// for a folder that holds none, then accepted for a 3.3.5a private server discovery does
/// not reach, and `status` with its scan once it is added.
#[cfg(test)]
fn first_run_fixtures(dir: &std::path::Path, t: &BTreeMap<String, Value>) {
    use common::trees::CustomRoot;

    let home = Home::new(&[]);
    // A scan first, as the panel's opening makes: the discovery cache then exists.
    home.run(&["status"]).expect(0);
    let status = write_fixture(dir, "status-none", &home.exec(&fill(t, "status", &[])), 0);
    assert_eq!(status["data"]["targets"], serde_json::json!([]), "{status}");
    let empty = home.env.root().join("empty");
    fs::create_dir_all(&empty).unwrap();
    let refused = write_fixture(
        dir,
        "targets-add-none",
        &home.exec(&fill(
            t,
            "targets_add",
            &[("path", &empty.display().to_string())],
        )),
        3,
    );
    assert_eq!(refused["error"]["code"], "target_not_found", "{refused}");
    let server = CustomRoot::at(home.env.root().join("pserver")).build();
    let added = write_fixture(
        dir,
        "targets-add",
        &home.exec(&fill(
            t,
            "targets_add",
            &[("path", &server.display().to_string())],
        )),
        0,
    );
    assert_eq!(added["command"], "targets add", "{added}");
    // The widget's cached read lists the client added since the last scan, live
    // (omad-jp9k): the discovery cache written before the add does not hold it.
    let status = write_fixture(dir, "status-added", &home.exec(&fill(t, "status", &[])), 0);
    let listed: Vec<(&str, bool)> = status["data"]["targets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| (c["display_name"].as_str().unwrap(), c["missing"] == true))
        .collect();
    assert_eq!(listed, [("Wrath 3.3.5a private server", false)], "{status}");
}

/// The client switcher's home (omad-widget-user-first-1n2p.20), laid out like the
/// development machine: Retail, Mists Classic, Classic Era, Anniversary and WoW: Forever
/// in one Battle.net root, and a 3.3.5a private-server client beside it. Foo is one
/// release behind on Retail and Classic Era, Mists Classic runs, and Anniversary was
/// uninstalled through Battle.net after it was registered, so `status` reports it
/// missing, with its plain `targets rm` remediation.
#[cfg(test)]
fn clients_fixtures(dir: &std::path::Path, t: &BTreeMap<String, Value>) {
    use common::trees::CustomRoot;

    let mut home = Home::new(&[
        "wow",
        "wow_classic",
        "wow_classic_era",
        "wow_anniversary",
        "wowf",
    ]);
    let pserver = home.env.root().join("pserver");
    home.with_custom(CustomRoot::at(pserver));
    let (old, new) = (Rel::new("Foo", "v1"), Rel::new("Foo", "v2"));
    home.installed_behind(
        support::FOO,
        ("v1", &old.assets()),
        ("v2", &new.assets()),
        &["retail", "era"],
    );
    home.exec(&fill(t, "check", &[])).expect(0);
    // Registered (a default is a registration), then the default back on Retail.
    home.run(&["targets", "default", "anniversary"]).expect(0);
    home.run(&["targets", "default", "retail"]).expect(0);
    home.uninstall("_anniversary_", "wow_anniversary");
    home.client_running("_classic_", "WoWClassic.exe");
    // A scan, so the cached clients `status --cached` reads know Anniversary is gone.
    home.run(&["status"]).expect(0);
    let status = write_fixture(
        dir,
        "status-clients",
        &home.exec(&fill(t, "status", &[])),
        0,
    );
    let clients: Vec<(&str, &str, bool, bool)> = status["data"]["targets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["id"].as_str().unwrap(),
                c["display_name"].as_str().unwrap(),
                c["missing"] == true,
                c["running"] == true,
            )
        })
        .collect();
    assert_eq!(clients.len(), 6, "{status}");
    for (id, missing, running) in [
        ("anniversary", true, false),
        ("classic", false, true),
        ("era", false, false),
        ("forever", false, false),
        ("retail", false, false),
        ("wotlk", false, false),
    ] {
        assert!(
            clients
                .iter()
                .any(|c| c.0 == id && c.2 == missing && c.3 == running),
            "{id}: {clients:?}"
        );
    }
    write_fixture(dir, "list-clients", &home.exec(&fill(t, "list", &[])), 0);
}

/// WeakAuras updated to 5.20.0 and a session played. `backup list` (plain and `-v`), an
/// `--addon` and a whole dry run, a stale token, a running client, and an ambiguous
/// addon after WeakAurasOptions moved to another addon.
#[cfg(test)]
fn backups_fixtures(dir: &std::path::Path, t: &BTreeMap<String, Value>) {
    let (mut home, _wtf, id) = restore_scene(t);
    write_fixture(dir, "backups", &home.exec(&fill(t, "backups", &[])), 0);
    let detail = fill(t, "backups_detail", &[("target", "retail")]);
    write_fixture(dir, "backups-detail", &home.exec(&detail), 0);
    let preview = |name: &str, slots: &[(&str, &str)]| {
        let mut argv = fill(t, name, slots);
        let at = argv.iter().position(|a| a == "backup").unwrap();
        argv.insert(at, "--dry-run".to_owned());
        argv
    };
    let addon = [
        ("target", "retail"),
        ("addon", "WeakAuras"),
        ("backup", id.as_str()),
    ];
    let whole = [("target", "retail"), ("backup", id.as_str())];
    let done = write_fixture(
        dir,
        "restore-addon-preview",
        &home.exec(&preview("restore_addon", &addon)),
        0,
    );
    assert!(done["data"]["precondition"].is_string(), "{done}");
    let done = write_fixture(
        dir,
        "restore-preview",
        &home.exec(&preview("restore", &whole)),
        0,
    );
    assert!(done["data"]["precondition"].is_string(), "{done}");
    // The newest checkpoint is never pruned: a newer backup first.
    home.exec(&fill(t, "backup_target", &[("target", "retail")]))
        .expect(0);
    let pruned = write_fixture(
        dir,
        "prune-preview",
        &home.exec(&preview("prune_backup", &whole)),
        0,
    );
    assert!(pruned["data"]["precondition"].is_string(), "{pruned}");
    let mut stale = fill(t, "restore", &whole);
    let at = stale.iter().position(|a| a == "backup").unwrap();
    stale.splice(
        at..at,
        ["--if-match".to_owned(), format!("pc1:{}", "0".repeat(64))],
    );
    let stale = write_fixture(dir, "restore-stale", &home.exec(&stale), 1);
    assert_eq!(stale["error"]["code"], "plan_stale", "{stale}");
    home.retail_running();
    let running = home.exec(&preview("restore_addon", &addon));
    let running = write_fixture(dir, "restore-running", &running, running.code);
    assert_eq!(running["error"]["code"], "game_running", "{running}");
    home.nothing_running();
    // WeakAurasOptions moved to another addon since the backup.
    install_zip(&home, "gh:o/WeakAuras", "5.21.0", &["WeakAuras"]);
    install_zip(&home, "gh:o/Opts", "1.0", &["WeakAurasOptions"]);
    let ambiguous = write_fixture(
        dir,
        "restore-ambiguous",
        &home.exec(&preview("restore_addon", &addon)),
        3,
    );
    assert_eq!(ambiguous["error"]["code"], "ref_ambiguous", "{ambiguous}");
}

/// The Issues view's envelopes: on retail, Foo (one release behind) with a Lua runtime
/// error and a custom message naming a character, a report received from another
/// player, and BugGrabber present; era without BugGrabber. `errors` strict, the same
/// file cut short (partial), Foo's raw details, a dismissal with a stale token, and
/// `status` in the `errors` state.
#[cfg(test)]
fn errors_fixtures(dir: &std::path::Path, t: &BTreeMap<String, Value>) {
    use common::addons::{addon_folder, toc};
    use common::buggrabber::{self, BugDb, BugEntry, NOW, TimeKind, write_files};

    let mut home = Home::new(&["wow", "wow_classic_era"]);
    let (old, new) = (Rel::new("Foo", "v1"), Rel::new("Foo", "v2"));
    home.installed_behind(
        support::FOO,
        ("v1", &old.assets()),
        ("v2", &new.assets()),
        &["retail"],
    );
    home.exec(&fill(t, "check", &[])).expect(0);
    let bug_toc = toc("120105", &["## SavedVariables: BugGrabberDB"]);
    addon_folder(
        &home.addons("_retail_"),
        "!BugGrabber",
        &[("!BugGrabber.toc", &bug_toc)],
        &[],
    );
    let at = |file: &str, line| buggrabber::stack_era_c("Foo", file, line);
    let db = BugDb::new(buggrabber::Writer::Modern, 10)
        .entry(BugEntry::new(
            &format!("{} FakeChar pulled too early", at("boss.lua", 3)),
            10,
            (TimeKind::Unix, NOW - 120),
        ))
        .entry(
            BugEntry::new(
                &format!(
                    "{} attempt to index a nil value (field 'x')",
                    at("core.lua", 12)
                ),
                10,
                (TimeKind::Unix, NOW - 60),
            )
            .stack(&[
                format!("{} in function 'Refresh'", at("core.lua", 12)),
                "[C]: in function 'xpcall'".to_owned(),
            ])
            .counter(4),
        )
        .entry(
            BugEntry::new(
                &format!("{} attempt to call a nil value", at("x.lua", 1)),
                10,
                (TimeKind::Unix, NOW - 30),
            )
            .source("FakeSender"),
        );
    let wtf = home.client("_retail_").join("WTF");
    let mut file = db.file();
    write_files(&wtf, std::slice::from_ref(&file));
    let now = [("OMADDONS_NOW", "2026-10-15T18:30:00Z")];
    // The service's errors refresh (F8) reads `stale` and `unchecked`, never `partial`.
    let errors_state = |name: &str| {
        let status = home.exec_with(&fill(t, "status", &[]), &now);
        let status = write_fixture(dir, name, &status, 0);
        let retail = status["data"]["targets"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == "retail")
            .unwrap();
        retail["errors"]["state"].as_str().unwrap().to_owned()
    };
    assert_eq!(errors_state("status-errors-unchecked"), "unchecked");
    let errors = home.exec_with(&fill(t, "errors", &[]), &now);
    let errors = write_fixture(dir, "errors", &errors, 0);
    assert_eq!(
        errors["data"]["targets"].as_array().unwrap().len(),
        2,
        "{errors}"
    );
    let raw = fill(t, "errors_raw", &[("target", "retail"), ("addon", "Foo")]);
    write_fixture(dir, "errors-raw", &home.exec_with(&raw, &now), 0);
    let status = write_fixture(
        dir,
        "status-errors",
        &home.exec_with(&fill(t, "status", &[]), &now),
        0,
    );
    assert_eq!(status["data"]["summary"]["state"], "errors", "{status}");
    let mut stale = fill(t, "dismiss_errors", &[("target", "retail")]);
    let at = stale.iter().position(|a| a == "errors").unwrap();
    stale.splice(
        at..at,
        ["--if-match".to_owned(), format!("pc1:{}", "0".repeat(64))],
    );
    let stale = write_fixture(dir, "dismiss-stale", &home.exec_with(&stale, &now), 1);
    assert_eq!(stale["error"]["code"], "plan_stale", "{stale}");
    // A dismissal that ran: its result line counts the errors it cleared.
    let dismiss = fill(t, "dismiss_errors", &[("target", "retail")]);
    let dismissed = write_fixture(dir, "dismiss", &home.exec_with(&dismiss, &now), 0);
    assert!(
        dismissed["data"]["targets"][0]["errors"]
            .as_u64()
            .is_some_and(|n| n > 0),
        "{dismissed}"
    );
    // The game still writing: a file cut short.
    let cut = file.bytes.len() - 20;
    file.bytes.truncate(cut);
    write_files(&wtf, &[file]);
    assert_eq!(errors_state("status-errors-stale"), "stale");
    write_fixture(
        dir,
        "errors-partial",
        &home.exec_with(&fill(t, "errors", &[]), &now),
        0,
    );
    assert_eq!(errors_state("status-errors-partial"), "partial");
    // Never checked (the update memo gone): the service's automatic check is due.
    fs::remove_file(home.memo()).unwrap();
    let due = write_fixture(
        dir,
        "status-check-due",
        &home.exec(&fill(t, "status", &[])),
        0,
    );
    assert_eq!(due["data"]["summary"]["check_due"], true, "{due}");
}

/// `$OMADDONS_WIDGET_OUT/fixtures/`: `status` in the `updates`, `idle` and `attention`
/// states, `list`, `search`, the `check` and `update_all` results (with their progress
/// lines), a `remove` preview, a `remove` whose token went stale, and a failure.
#[test]
#[ignore = "run by the widget pipeline, which sets OMADDONS_WIDGET_OUT"]
fn write_widget_fixtures() {
    let dir = out_dir().join("fixtures");
    fs::create_dir_all(&dir).unwrap();
    let t = templates();

    // Updates: Foo and Bar one release behind on retail and era, Baz current on retail.
    let mut home = Home::new(&["wow", "wow_classic_era"]);
    let baz = Rel::new("Baz", "v1");
    gh_release(&home.http, support::BAZ, "v1", &baz.assets());
    gh_latest(&home.http, support::BAZ, "v1");
    home.run(&["install", "gh:o/baz", "-t", "retail"]).expect(0);
    for (base, name) in [(support::FOO, "Foo"), (support::BAR, "Bar")] {
        let (old, new) = (Rel::new(name, "v1"), Rel::new(name, "v2"));
        home.installed_behind(
            base,
            ("v1", &old.assets()),
            ("v2", &new.assets()),
            &["retail", "era"],
        );
    }
    // A folder omaddons does not manage, for `list`.
    let loose = home.addons("_retail_").join("Loose");
    fs::create_dir_all(&loose).unwrap();
    fs::write(
        loose.join("Loose.toc"),
        "## Interface: 120105\n## Title: Loose\n",
    )
    .unwrap();
    write_fixture(&dir, "check", &home.exec(&fill(&t, "check", &[])), 0);
    let status = write_fixture(
        &dir,
        "status-updates",
        &home.exec(&fill(&t, "status", &[])),
        0,
    );
    assert_eq!(status["data"]["summary"]["state"], "updates", "{status}");
    home.retail_running();
    let status = write_fixture(
        &dir,
        "status-running",
        &home.exec(&fill(&t, "status", &[])),
        0,
    );
    assert_eq!(
        status["data"]["summary"]["running"][0], "retail",
        "{status}"
    );
    home.nothing_running();
    write_fixture(&dir, "list", &home.exec(&fill(&t, "list", &[])), 0);
    let mut preview = fill(&t, "remove", &[("target", "retail"), ("addon", "Baz")]);
    preview.insert(preview.len() - 5, "--dry-run".to_owned());
    let preview = write_fixture(&dir, "remove-preview", &home.exec(&preview), 0);
    assert!(preview["data"]["precondition"].is_string(), "{preview}");
    let mut stale = fill(&t, "remove", &[("target", "retail"), ("addon", "Baz")]);
    stale.splice(
        stale.len() - 5..stale.len() - 5,
        ["--if-match".to_owned(), format!("pc1:{}", "0".repeat(64))],
    );
    let stale = write_fixture(&dir, "remove-stale", &home.exec(&stale), 1);
    assert_eq!(stale["error"]["code"], "plan_stale", "{stale}");
    write_fixture(&dir, "update", &home.exec(&fill(&t, "update_all", &[])), 0);
    let status = write_fixture(
        &dir,
        "status-after-update",
        &home.exec(&fill(&t, "status", &[])),
        0,
    );
    // "Undo last" previews the CLI's own undo argv.
    let undo: Vec<String> = status["data"]["last_change"]["undo"]["argv"]
        .as_array()
        .unwrap()
        .iter()
        .skip(1)
        .map(|a| a.as_str().unwrap().to_owned())
        .collect();
    let mut preview = support::standard(true);
    preview.push("--dry-run".to_owned());
    preview.extend(undo.iter().cloned());
    let preview = write_fixture(&dir, "undo-preview", &home.exec(&preview), 0);
    assert!(preview["data"]["precondition"].is_string(), "{preview}");
    let failure = write_fixture(
        &dir,
        "failure",
        &home.exec(&fill(&t, "update_target", &[("target", "nosuch")])),
        3,
    );
    assert_eq!(failure["error"]["code"], "target_not_found", "{failure}");
    // The same undo, run: its result line names what it reverted.
    let mut run = support::standard(true);
    run.extend(undo);
    let undone = write_fixture(&dir, "undo", &home.exec(&run), 0);
    assert!(undone["data"]["reverted"]["kind"].is_string(), "{undone}");

    // The Installed view's markers: Foo edited by hand, Bar's folder gone, Baz pinned
    // behind its latest; and Foo's release notes, which the Updates view's `i` reads.
    let mut home = Home::new(&["wow"]);
    for (base, name) in [
        (support::FOO, "Foo"),
        (support::BAR, "Bar"),
        (support::BAZ, "Baz"),
    ] {
        let (old, new) = (Rel::new(name, "v1"), Rel::new(name, "v2"));
        home.installed_behind(
            base,
            ("v1", &old.assets()),
            ("v2", &new.assets()),
            &["retail"],
        );
    }
    home.exec(&fill(&t, "check", &[])).expect(0);
    home.exec(&fill(&t, "pin", &[("target", "retail"), ("addon", "Baz")]))
        .expect(0);
    let toc = home.addons("_retail_").join("Foo/Foo.toc");
    fs::write(&toc, "## Interface: 120105\n## Title: edited by hand\n").unwrap();
    fs::remove_dir_all(home.addons("_retail_").join("Bar")).unwrap();
    let markers = write_fixture(&dir, "list-markers", &home.exec(&fill(&t, "list", &[])), 0);
    let drift: Vec<(&str, &str, &str)> = markers["data"]["targets"][0]["addons"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| {
            (
                a["name"].as_str().unwrap(),
                a["drift"]["state"].as_str().unwrap(),
                a["latest"]["status"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        drift,
        [
            ("Bar", "missing", "update_available"),
            ("Baz", "clean", "pinned"),
            ("Foo", "modified", "update_available"),
        ],
        "{markers}"
    );
    gh_feed(
        &home.http,
        support::FOO,
        &[
            ("v2", "<p>Fixes the <b>pull</b> timer.</p>"),
            ("v1", "<p>First.</p>"),
        ],
        None,
    );
    let changelog = fill(&t, "changelog", &[("target", "retail"), ("addon", "Foo")]);
    write_fixture(&dir, "changelog", &home.exec(&changelog), 0);

    errors_fixtures(&dir, &t);
    backups_fixtures(&dir, &t);
    plans_fixtures(&dir, &t);
    repair_fixtures(&dir, &t);
    clients_fixtures(&dir, &t);
    first_run_fixtures(&dir, &t);
    adopt_fixtures(&dir, &t);

    // Idle: Baz current on retail, checked just now.
    let home = Home::new(&["wow"]);
    gh_release(&home.http, support::BAZ, "v1", &baz.assets());
    gh_latest(&home.http, support::BAZ, "v1");
    home.run(&["install", "gh:o/baz", "-t", "retail"]).expect(0);
    home.exec(&fill(&t, "check", &[])).expect(0);
    let status = write_fixture(&dir, "status-idle", &home.exec(&fill(&t, "status", &[])), 0);
    assert_eq!(status["data"]["summary"]["state"], "idle", "{status}");

    // Attention: Baz's folder deleted behind omaddons' back (a P1 issue).
    fs::remove_dir_all(home.addons("_retail_").join("Baz")).unwrap();
    let status = write_fixture(
        &dir,
        "status-attention",
        &home.exec(&fill(&t, "status", &[])),
        0,
    );
    assert_eq!(status["data"]["summary"]["state"], "attention", "{status}");

    // Search: Details! and Recount on WoWInterface, Skada on GitHub, ElvUI on Tukui.
    let home = Home::new(&["wow"]);
    let mut details = FileEntry::new(23056, "Details! Damage Meter", &["Details"]);
    details.downloads = 515_870;
    let mut recount = FileEntry::new(7000, "Recount Damage Meter", &["Recount"]);
    recount.downloads = 2_100_000;
    recount.compat = vec![("12.0.5".into(), "Midnight".into())];
    wowi_filelist(&home.http, &[details, recount]);
    tukui_list(&home.http, &[elvui()]);
    github_search(
        &home.http,
        "damage meter",
        &[(
            "Skada-Damage-Meter",
            "Skada",
            "A modular damage meter for World of Warcraft",
            412,
            "2026-09-01T00:00:00Z",
        )],
    );
    let search = fill(
        &t,
        "search",
        &[("target", "retail"), ("query", "damage meter")],
    );
    let search = write_fixture(&dir, "search", &home.exec(&search), 0);
    assert!(
        !search["data"]["results"].as_array().unwrap().is_empty(),
        "{search}"
    );

    // Search under All (1n2p.25): a retail and a Classic Era client. Questie's catalog
    // entry declares vanilla and not mainline, so it is `yes` on era and `no` on retail;
    // a GitHub hit nothing has evidence for is `unknown` on both.
    let home = Home::new(&["wow", "wow_classic_era"]);
    wowi_filelist(&home.http, &[]);
    tukui_list(&home.http, &[]);
    github_search(
        &home.http,
        "questie",
        &[(
            "someone",
            "QuestieExtras",
            "A World of Warcraft addon: more quests for Questie",
            12,
            "2026-09-01T00:00:00Z",
        )],
    );
    let search_all = fill(&t, "search_all", &[("query", "questie")]);
    let search_all = write_fixture(&dir, "search-all", &home.exec(&search_all), 0);
    let results = search_all["data"]["results"].as_array().unwrap();
    let compat = |name: &str| {
        results
            .iter()
            .find(|r| r["name"] == name)
            .map(|r| r["compat"].clone())
    };
    assert_eq!(
        compat("Questie"),
        Some(serde_json::json!({"era": "yes", "retail": "no"})),
        "{search_all}"
    );
    assert_eq!(
        compat("QuestieExtras"),
        Some(serde_json::json!({"era": "unknown", "retail": "unknown"})),
        "{search_all}"
    );
    // The catalog an empty Search box shows (1n2p.26): local and offline, for All and
    // for one client; neither read reaches the network.
    let requests = common::netlog::net_log(&home).len();
    let catalog = write_fixture(
        &dir,
        "search-catalog",
        &home.exec(&fill(&t, "catalog", &[])),
        0,
    );
    let era = write_fixture(
        &dir,
        "search-catalog-era",
        &home.exec(&fill(&t, "catalog_target", &[("target", "era")])),
        0,
    );
    assert_eq!(
        common::netlog::net_log(&home).len(),
        requests,
        "the catalog asks no source"
    );
    for (envelope, client) in [(&catalog, "retail"), (&era, "era")] {
        let questie = envelope["data"]["results"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["name"] == "Questie")
            .map(|r| r["compat"][client].clone());
        assert_eq!(
            questie,
            Some(if client == "era" {
                "yes".into()
            } else {
                "no".into()
            }),
            "{envelope}"
        );
    }

    states_fixtures(&dir, &t);

    // The published contract: the inputs of omarchy/tests/contract-reads.test.js, which
    // holds every key the widget reads to a key these schemas declare (H1's guard). Not
    // reducer fixtures; `errors.json` is already the Lua errors envelope.
    let schemas = write_fixture(
        &dir,
        "contract-schemas",
        &home.run(&["robot-docs", "schema"]),
        0,
    );
    assert!(
        schemas["data"]["commands"].is_object() && schemas["data"]["envelope"].is_object(),
        "{schemas}"
    );
    let errors = write_fixture(
        &dir,
        "contract-errors",
        &home.run(&["robot-docs", "errors"]),
        0,
    );
    assert!(
        errors["data"]["error_codes"].is_array() && errors["data"]["warning_codes"].is_array(),
        "{errors}"
    );
}
